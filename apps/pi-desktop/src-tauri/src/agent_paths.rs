use std::path::PathBuf;

pub(crate) fn agent_dir() -> Result<PathBuf, String> {
    if let Some(configured) = std::env::var_os("PI_AGENT_DIR") {
        return Ok(PathBuf::from(configured));
    }
    home_dir()
        .map(|home| home.join(".pi").join("agent"))
        .ok_or_else(|| "cannot determine Pi agent directory; set PI_AGENT_DIR".to_string())
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}
