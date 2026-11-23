use std::borrow::Cow;

use pi_plugin::ToolError;
use pi_tool_support::execution;
use serde::Deserialize;
use unicode_normalization::UnicodeNormalization as _;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct EditInput {
    pub old_text: String,
    pub new_text: String,
}

pub(super) struct Replacement {
    pub(super) index: usize,
    pub(super) length: usize,
    pub(super) new_text: String,
}

pub(super) struct AppliedEdits {
    pub new_content: String,
    pub replacements: Vec<Replacement>,
}

pub(super) fn normalize_lf_cow(text: &str) -> Cow<'_, str> {
    if text.contains('\r') {
        Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(text)
    }
}

pub(super) fn normalize_lf(text: &str) -> String {
    normalize_lf_cow(text).into_owned()
}

fn fuzzy(text: &str) -> String {
    text.nfkc()
        .collect::<String>()
        .split('\n')
        .map(|line| {
            line.trim_end_matches(|c| {
                matches!(c, '\u{0009}'..='\u{000d}' | ' ' | '\u{00a0}' | '\u{1680}'
                    | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}'
                    | '\u{205f}' | '\u{3000}' | '\u{feff}')
            })
        })
        .collect::<Vec<_>>()
        .join("\n")
        .replace(['\u{2018}', '\u{2019}', '\u{201a}', '\u{201b}'], "'")
        .replace(['\u{201c}', '\u{201d}', '\u{201e}', '\u{201f}'], "\"")
        .replace(
            [
                '\u{2010}', '\u{2011}', '\u{2012}', '\u{2013}', '\u{2014}', '\u{2015}', '\u{2212}',
            ],
            "-",
        )
        .replace(
            [
                '\u{00a0}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}', '\u{2007}',
                '\u{2008}', '\u{2009}', '\u{200a}', '\u{202f}', '\u{205f}', '\u{3000}',
            ],
            " ",
        )
}

fn fuzzy_is_identity(text: &str) -> bool {
    if !text.is_ascii() {
        return false;
    }
    let bytes = text.as_bytes();
    bytes.iter().enumerate().all(|(index, byte)| {
        if *byte != b'\n' {
            return true;
        }
        index == 0 || !matches!(bytes[index - 1], b'\t'..=b'\r' | b' ')
    }) && bytes
        .last()
        .is_none_or(|byte| *byte == b'\n' || !matches!(*byte, b'\t'..=b'\r' | b' '))
}

fn fuzzy_view(text: &str) -> Cow<'_, str> {
    if fuzzy_is_identity(text) {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(fuzzy(text))
    }
}

fn apply_replacements(content: &str, replacements: &[Replacement], offset: usize) -> String {
    let mut result = content.to_string();
    for replacement in replacements.iter().rev() {
        let start = replacement.index - offset;
        result.replace_range(start..start + replacement.length, &replacement.new_text);
    }
    result
}

fn line_spans(content: &str) -> Vec<std::ops::Range<usize>> {
    let mut offset = 0;
    content
        .split_inclusive('\n')
        .map(|line| {
            let start = offset;
            offset += line.len();
            start..offset
        })
        .collect()
}

/// Fuzzy offsets belong to a normalized view. Widen only the touched line groups
/// before mapping them back, so duplicate normalized lines cannot be misaligned.
fn widen_to_lines(
    original: &str,
    base: &str,
    replacements: Vec<Replacement>,
) -> Result<Vec<Replacement>, ToolError> {
    let original_lines = line_spans(original);
    let base_lines = line_spans(base);
    if original_lines.len() != base_lines.len() {
        return Err(execution(
            "Cannot preserve unchanged lines because the base content has a different line count.",
        ));
    }
    let mut groups: Vec<(usize, usize, Vec<Replacement>)> = Vec::new();
    for replacement in replacements {
        let start = base_lines
            .iter()
            .position(|line| line.contains(&replacement.index))
            .ok_or_else(|| execution("Replacement range is outside the base content."))?;
        let end = base_lines[start..]
            .iter()
            .position(|line| line.end >= replacement.index + replacement.length)
            .map(|end| start + end + 1)
            .ok_or_else(|| execution("Replacement range is outside the base content."))?;
        if let Some((_, group_end, group)) = groups.last_mut()
            && start < *group_end
        {
            *group_end = (*group_end).max(end);
            group.push(replacement);
        } else {
            groups.push((start, end, vec![replacement]));
        }
    }
    Ok(groups
        .into_iter()
        .map(|(start, end, group)| {
            let base_start = base_lines[start].start;
            let original_start = original_lines[start].start;
            Replacement {
                index: original_start,
                length: original_lines[end - 1].end - original_start,
                new_text: apply_replacements(
                    &base[base_start..base_lines[end - 1].end],
                    &group,
                    base_start,
                ),
            }
        })
        .collect())
}

pub(super) fn apply_edits(
    content: &str,
    edits: &[EditInput],
    path: &str,
) -> Result<AppliedEdits, ToolError> {
    let edits = edits
        .iter()
        .map(|edit| EditInput {
            old_text: normalize_lf(&edit.old_text),
            new_text: normalize_lf(&edit.new_text),
        })
        .collect::<Vec<_>>();
    for (i, edit) in edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(execution(if edits.len() == 1 {
                format!("oldText must not be empty in {path}.")
            } else {
                format!("edits[{i}].oldText must not be empty in {path}.")
            }));
        }
    }
    let fuzzy_content = fuzzy_view(content);
    let use_fuzzy = edits.iter().any(|edit| {
        !content.contains(&edit.old_text)
            && fuzzy_content.contains(fuzzy_view(&edit.old_text).as_ref())
    });
    let base = if use_fuzzy {
        fuzzy_content.as_ref()
    } else {
        content
    };
    let mut matched = Vec::new();
    for (i, edit) in edits.iter().enumerate() {
        let fuzzy_old = fuzzy_view(&edit.old_text);
        // Exact-first still applies when another edit selected the fuzzy view.
        let found = base
            .find(&edit.old_text)
            .map(|index| (index, edit.old_text.len()))
            .or_else(|| {
                fuzzy_content
                    .find(fuzzy_old.as_ref())
                    .map(|index| (index, fuzzy_old.len()))
            });
        let Some((index, length)) = found else {
            return Err(execution(if edits.len() == 1 {
                format!(
                    "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
                )
            } else {
                format!(
                    "Could not find edits[{i}] in {path}. The oldText must match exactly including all whitespace and newlines."
                )
            }));
        };
        // Pi counts fuzzy-equivalent occurrences even when the match was exact.
        let count = if fuzzy_old.is_empty() {
            fuzzy_content.encode_utf16().count().saturating_sub(1)
        } else {
            fuzzy_content.matches(fuzzy_old.as_ref()).count()
        };
        if count > 1 {
            return Err(execution(if edits.len() == 1 {
                format!(
                    "Found {count} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique."
                )
            } else {
                format!(
                    "Found {count} occurrences of edits[{i}] in {path}. Each oldText must be unique. Please provide more context to make it unique."
                )
            }));
        }
        matched.push((
            i,
            Replacement {
                index,
                length,
                new_text: edit.new_text.clone(),
            },
        ));
    }
    matched.sort_by_key(|(_, r)| r.index);
    for pair in matched.windows(2) {
        if pair[0].1.index + pair[0].1.length > pair[1].1.index {
            return Err(execution(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                pair[0].0, pair[1].0
            )));
        }
    }
    let replacements = matched.into_iter().map(|(_, r)| r).collect();
    let replacements = if use_fuzzy {
        widen_to_lines(content, base, replacements)?
    } else {
        replacements
    };
    let new_content = apply_replacements(content, &replacements, 0);
    if new_content == content {
        return Err(execution(if edits.len() == 1 {
            format!(
                "No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
            )
        } else {
            format!("No changes made to {path}. The replacements produced identical content.")
        }));
    }
    Ok(AppliedEdits {
        new_content,
        replacements,
    })
}

/// Splice LF-view ranges into the original bytes; only inserted line breaks use
/// the starting line's ending. A CR/LF seam must not merge two logical breaks.
pub(super) fn apply_to_original(content: &str, replacements: &[Replacement]) -> String {
    let mut crlf_before = 0;
    let mut next_crlf = content.find("\r\n");
    let mut to_original = |offset: usize| {
        while let Some(next) = next_crlf
            && next - crlf_before < offset
        {
            crlf_before += 1;
            next_crlf = content[next + 2..].find("\r\n").map(|i| next + 2 + i);
        }
        offset + crlf_before
    };
    let mut result = String::new();
    let mut push = |part: &str| {
        if result.ends_with('\r') && part.starts_with('\n') {
            result.push('\n');
        }
        result.push_str(part);
    };
    let mut position = 0;
    for replacement in replacements {
        let start = to_original(replacement.index);
        push(&content[position..start]);
        let line_feed = content[start..]
            .find('\n')
            .map(|i| start + i)
            .or_else(|| content[..start].rfind('\n'));
        let ending = match line_feed {
            Some(i) if i > 0 && content.as_bytes()[i - 1] == b'\r' => "\r\n",
            Some(_) => "\n",
            None if content.contains('\r') => "\r",
            None => "\n",
        };
        push(&replacement.new_text.replace('\n', ending));
        position = to_original(replacement.index + replacement.length);
    }
    push(&content[position..]);
    result
}
