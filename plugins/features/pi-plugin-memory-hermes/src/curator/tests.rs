use super::*;

fn fixture() -> (tempfile::TempDir, Curator) {
    let temp = tempfile::tempdir().unwrap();
    let curator = Curator::new(temp.path().join("skills"), Config::default());
    fs::create_dir_all(&curator.root).unwrap();
    (temp, curator)
}

fn skill(curator: &Curator, name: &str, at: i64, managed: bool) -> PathBuf {
    let dir = curator.root.join(name);
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: reusable procedure\n---\nRun scripts/check.sh"),
    )
    .unwrap();
    fs::write(dir.join("scripts/check.sh"), b"#!/bin/sh\nprintf ok\n").unwrap();
    Metadata::new(&dir, managed, "user", at)
        .unwrap()
        .save(&dir)
        .unwrap();
    dir
}

#[test]
fn transitions_follow_activity_and_preview_changes_no_bytes() {
    let (_temp, c) = fixture();
    let recent = skill(&c, "recent", 99 * DAY, true);
    let stale = skill(&c, "stale", 70 * DAY, true);
    let old = skill(&c, "old", 10 * DAY, true);
    let before = fs::read(stale.join(metadata::FILE)).unwrap();
    let report = c.prune(100 * DAY, true).unwrap();
    assert_eq!(report.planned.len(), 2);
    assert!(report.completed.is_empty());
    assert_eq!(fs::read(stale.join(metadata::FILE)).unwrap(), before);
    assert!(old.exists());
    assert!(!c.root.join(".backups").exists());
    assert!(!c.root.join(".curator-state.json").exists());
    let report = c.prune(100 * DAY, false).unwrap();
    assert_eq!(report.completed.len(), 2);
    assert!(report.backup.is_some());
    assert_eq!(Metadata::read(&stale).unwrap().state, State::Stale);
    assert!(recent.exists());
    assert!(!old.exists());
    let mut m = Metadata::read(&stale).unwrap();
    m.last_activity_at = 100 * DAY;
    m.save(&stale).unwrap();
    c.prune(100 * DAY, false).unwrap();
    assert_eq!(Metadata::read(&stale).unwrap().state, State::Active);
}

#[test]
fn only_managed_unpinned_unchanged_packages_can_be_curated() {
    let (_temp, c) = fixture();
    skill(&c, "user", 0, false);
    let pinned = skill(&c, "pinned", 0, true);
    c.pin("pinned", true).unwrap();
    let changed = skill(&c, "changed", 0, true);
    fs::write(changed.join("scripts/check.sh"), "user edit").unwrap();
    assert!(c.plan(100 * DAY).unwrap().planned.is_empty());
    assert!(c.archive("pinned").is_err());
    assert!(c.archive("user").is_err());
    assert!(c.archive("changed").is_err());
    assert!(pinned.exists());
    c.adopt("changed").unwrap();
    assert_eq!(
        c.plan(chrono::Utc::now().timestamp_millis())
            .unwrap()
            .planned
            .len(),
        0
    );
}

#[test]
fn archive_and_restore_preserve_the_complete_package_and_refuse_collisions() {
    let (_temp, c) = fixture();
    let dir = skill(&c, "workflow", 0, true);
    fs::write(dir.join("asset.bin"), [0, 255, 1, 128]).unwrap();
    c.adopt("workflow").unwrap();
    let before = metadata::fingerprint(&dir).unwrap();
    let id = c.archive("workflow").unwrap();
    assert!(!dir.exists());
    assert_eq!(c.archives().unwrap().len(), 1);
    fs::create_dir(&dir).unwrap();
    assert!(c.restore(&id).unwrap_err().to_string().contains("exists"));
    fs::remove_dir(&dir).unwrap();
    assert_eq!(c.restore(&id).unwrap(), "workflow");
    assert_eq!(metadata::fingerprint(&dir).unwrap(), before);
    assert!(c.archives().unwrap().is_empty());
    assert!(
        c.plan(chrono::Utc::now().timestamp_millis())
            .unwrap()
            .planned
            .is_empty()
    );
}

#[test]
fn rollback_restores_before_images_and_preserves_newer_external_changes() {
    let (_temp, c) = fixture();
    let dir = skill(&c, "workflow", 0, true);
    let before = metadata::fingerprint(&dir).unwrap();
    let report = c.prune(100 * DAY, false).unwrap();
    let id = report.backup.unwrap();
    c.seal_backup(&id, &[]).unwrap();
    c.rollback(Some(&id)).unwrap();
    assert_eq!(metadata::fingerprint(&dir).unwrap(), before);
    fs::write(dir.join("SKILL.md"), "external work").unwrap();
    assert!(c.rollback(Some(&id)).is_err());
    assert_eq!(
        fs::read_to_string(dir.join("SKILL.md")).unwrap(),
        "external work"
    );
}

#[test]
fn scheduler_defers_first_run_and_respects_activity_pause_and_intervals() {
    let (_temp, c) = fixture();
    assert!(!c.due(0).unwrap());
    assert!(!c.due(7 * DAY - 1).unwrap());
    assert!(c.due(7 * DAY).unwrap());
    c.update_state(|s| s.last_activity_at = Some(7 * DAY))
        .unwrap();
    assert!(!c.due(7 * DAY).unwrap());
    assert!(c.due(7 * DAY + 7_200_000).unwrap());
    c.update_state(|s| s.paused = true).unwrap();
    assert!(!c.due(20 * DAY).unwrap());
    fs::write(c.root.join(".curator-state.json"), "bad JSON").unwrap();
    assert!(c.due(20 * DAY).is_err());
}

#[test]
fn local_cli_preview_is_read_only_and_uses_the_selected_profile_root() {
    let temp = tempfile::tempdir().unwrap();
    let output = execute_local(
        temp.path(),
        temp.path(),
        false,
        &["run".into(), "--dry-run".into()],
    )
    .unwrap()
    .unwrap();
    assert!(output.contains("\"dry_run\": true"));
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    fs::write(temp.path().join("memory.json"), r#"{"providers":{"hermes":{"memoryDir":"custom-memory","curator":{"consolidate":true,"intervalHours":72}}}}"#).unwrap();
    assert!(
        execute_local(temp.path(), temp.path(), false, &["run".into()])
            .unwrap()
            .is_none()
    );
    let status: serde_json::Value = serde_json::from_str(
        &execute_local(temp.path(), temp.path(), false, &["status".into()])
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(status["config"]["intervalHours"], 72);
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
}

#[test]
fn scoped_commands_isolate_same_named_skills_archives_backups_and_trust() {
    let temp = tempfile::tempdir().unwrap();
    let agent = temp.path().join("agent");
    let repo = temp.path().join("repo");
    fs::create_dir_all(repo.join(".git")).unwrap();
    let global = Curator::new(agent.join("pi-hermes-memory/skills"), Config::default());
    let project = Curator::new(repo.join(".hermes/skills"), Config::default());
    let global_skill = skill(&global, "workflow", 0, true);
    let project_skill = skill(&project, "workflow", 0, true);
    let user = skill(&project, "manual", 0, false);
    let pinned = skill(&project, "pinned", 0, true);
    project.pin("pinned", true).unwrap();
    let changed = skill(&project, "changed", 0, true);
    fs::write(changed.join("scripts/check.sh"), "user changes").unwrap();
    let legacy = skill(&project, "legacy", 0, true);
    fs::remove_file(legacy.join(metadata::FILE)).unwrap();
    let external = Curator::new(repo.join(".agents/skills"), Config::default());
    let external_skill = skill(&external, "external", 0, true);
    let historical = Curator::new(
        temp.path().join("another/.hermes/skills"),
        Config::default(),
    );
    let historical_skill = skill(&historical, "historical", 0, true);
    let command = |trusted, args: &[&str]| {
        execute_local(
            &agent,
            &repo,
            trusted,
            &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    };
    let preview = command(true, &["run", "--dry-run"]).unwrap().unwrap();
    let preview: serde_json::Value = serde_json::from_str(&preview).unwrap();
    assert_eq!(preview["global"]["planned"].as_array().unwrap().len(), 1);
    assert_eq!(
        preview["project:repo"]["planned"].as_array().unwrap().len(),
        1
    );
    assert!(!project.root.join(".curator-state.json").exists());
    assert!(!project.root.join(".backups").exists());
    assert!(command(false, &["run", "--scope", "project"]).is_err());
    assert!(command(true, &["adopt", "project:another:workflow"]).is_err());
    assert!(command(true, &["archive", "global:workflow", "--scope", "project"]).is_err());
    command(true, &["pin", "workflow"]).unwrap();
    assert!(Metadata::read(&global_skill).unwrap().pinned);
    assert!(!Metadata::read(&project_skill).unwrap().pinned);
    command(true, &["run", "--scope", "project"]).unwrap();
    assert!(!project_skill.exists());
    assert!(global_skill.exists());
    for path in [
        &user,
        &pinned,
        &changed,
        &legacy,
        &external_skill,
        &historical_skill,
    ] {
        assert!(path.exists());
    }
    let archives: serde_json::Value = serde_json::from_str(
        &command(true, &["list-archived", "--scope", "project"])
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let id = archives["project:repo"][0]["id"].as_str().unwrap();
    assert!(id.starts_with("project:repo:"));
    command(true, &["restore", id]).unwrap();
    assert!(project_skill.exists());
    assert!(Metadata::read(&global_skill).unwrap().pinned);
    command(true, &["archive", "project:repo:workflow"]).unwrap();
    let backups: serde_json::Value = serde_json::from_str(
        &command(true, &["rollback", "--list", "--scope", "project"])
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let backup = backups["project:repo"][0].as_str().unwrap();
    command(true, &["rollback", "--id", backup]).unwrap();
    assert!(project_skill.exists());
    assert!(Metadata::read(&global_skill).unwrap().pinned);
    assert!(global.backups().unwrap().is_empty());
    command(true, &["pause", "--scope", "project"]).unwrap();
    assert!(project.state().unwrap().paused);
    assert!(!global.state().unwrap().paused);
    command(true, &["adopt", "project:repo:manual"]).unwrap();
    assert!(Metadata::read(&user).unwrap().curator_managed);
}

#[test]
fn run_claims_exclude_other_process_handles() {
    let (_temp, c) = fixture();
    let claim = metadata::lock(&c.root, ".curator-run.lock", false).unwrap();
    assert!(metadata::lock(&c.root, ".curator-run.lock", false).is_err());
    drop(claim);
    assert!(metadata::lock(&c.root, ".curator-run.lock", false).is_ok());
}

#[test]
fn foreground_leases_block_other_sessions_and_release_on_exit() {
    let (_temp, c) = fixture();
    assert!(!metadata::foreground_active(&c.root).unwrap());
    let lease = metadata::ActivityLease::acquire(&c.root).unwrap();
    assert!(metadata::foreground_active(&c.root).unwrap());
    drop(lease);
    assert!(!metadata::foreground_active(&c.root).unwrap());
    let stale = metadata::lock(&c.root.join(".curator-active"), "crashed-process", false).unwrap();
    drop(stale);
    assert!(!metadata::foreground_active(&c.root).unwrap());
}

#[test]
fn rollback_does_not_touch_pins_or_unrelated_new_skills() {
    let (_temp, c) = fixture();
    skill(&c, "workflow", 0, true);
    let pinned = skill(&c, "pinned", 0, true);
    c.pin("pinned", true).unwrap();
    let before_pin = fs::read(pinned.join(metadata::FILE)).unwrap();
    let backup = c.backup().unwrap();
    let unrelated = skill(&c, "new-user-request", 10 * DAY, true);
    c.seal_backup(&backup, &[]).unwrap();
    c.rollback(Some(&backup)).unwrap();
    assert!(unrelated.exists());
    assert_eq!(fs::read(pinned.join(metadata::FILE)).unwrap(), before_pin);
}

#[test]
fn explicit_usage_keeps_the_matching_skill_active_without_adopting_it() {
    use pi_plugin_skills::{SkillActivityObserver, SkillInfo};
    let (_temp, c) = fixture();
    let dir = skill(&c, "workflow", 0, false);
    ActivityObserver(vec![c.root.clone()]).used(&SkillInfo {
        name: "workflow".into(),
        description: String::new(),
        content: String::new(),
        file_path: dir.join("SKILL.md"),
        disable_model_invocation: false,
    });
    let m = Metadata::read(&dir).unwrap();
    assert_eq!(m.use_count, 1);
    assert!(!m.curator_managed);
    assert!(m.last_activity_at > 0);
}

#[test]
fn project_activity_updates_only_the_matching_package() {
    use pi_plugin_skills::SkillInfo;
    let (temp, global) = fixture();
    let project = Curator::new(temp.path().join("repo/.hermes/skills"), Config::default());
    let external = Curator::new(temp.path().join("repo/.agents/skills"), Config::default());
    let global_skill = skill(&global, "workflow", 0, true);
    let project_skill = skill(&project, "workflow", 0, true);
    let external_skill = skill(&external, "workflow", 0, true);
    let observer = activity_observer(vec![global.root.clone(), project.root.clone()]);
    for path in [&project_skill, &external_skill] {
        observer.used(&SkillInfo {
            name: "workflow".into(),
            description: String::new(),
            content: String::new(),
            file_path: path.join("SKILL.md"),
            disable_model_invocation: false,
        });
        for root in [&global.root, &project.root] {
            observe_read(root, &path.join("SKILL.md"));
        }
    }
    let m = Metadata::read(&project_skill).unwrap();
    assert_eq!(m.use_count, 1);
    assert_eq!(m.view_count, 1);
    assert!(m.last_activity_at > 0);
    assert_eq!(Metadata::read(&global_skill).unwrap().last_activity_at, 0);
    assert_eq!(Metadata::read(&external_skill).unwrap().last_activity_at, 0);
    assert!(
        project
            .plan(chrono::Utc::now().timestamp_millis())
            .unwrap()
            .planned
            .is_empty()
    );
}

#[test]
fn hostile_identifiers_and_linked_packages_are_rejected() {
    let (_temp, c) = fixture();
    assert!(c.adopt("../outside").is_err());
    assert!(c.restore("../outside").is_err());
    assert!(c.rollback(Some("../outside")).is_err());
    #[cfg(unix)]
    {
        let dir = skill(&c, "workflow", 0, true);
        std::os::unix::fs::symlink("/tmp", dir.join("linked")).unwrap();
        assert!(c.archive("workflow").is_err());
        assert!(c.plan(100 * DAY).unwrap().planned.is_empty());
    }
}
