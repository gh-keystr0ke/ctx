use std::{
    fmt::Write as _,
    io::{self, BufRead, Write},
    path::Path,
};

use super::{
    Result, fetch,
    manifest::{Category, Evidence, EvidenceKind, Label, Point, Project},
    project_path, read_project, write_yaml,
};

pub(super) fn review(
    root: &Path,
    name: &str,
    reviewer: &str,
    second: bool,
    point: Option<&str>,
) -> Result<()> {
    let mut project = read_project(root, name)?;
    let checkout = fetch::checked_out(&fetch::cache_root()?, &project)?;
    let indexes = review_indexes(&project, second, point);
    let path = project_path(root, name)?;
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut output = io::stdout().lock();
    for index in indexes {
        let point = &mut project.points[index];
        let code = code_excerpt(&checkout, &point.location)?;
        show(&mut output, point, &code, second)?;
        match edit_point(&mut input, &mut output, point, reviewer, second)? {
            Progress::Save => write_yaml(&path, &project)?,
            Progress::Skip => {}
            Progress::Quit => break,
        }
    }
    Ok(())
}

fn review_indexes(project: &Project, second: bool, requested: Option<&str>) -> Vec<usize> {
    if second {
        let mut eligible: Vec<_> = project
            .points
            .iter()
            .enumerate()
            .filter(|(_, point)| point.label.is_some() && point.verified_by.is_some())
            .collect();
        eligible.sort_by_cached_key(|(_, point)| {
            let key = format!("{}second{}", project.sample.seed, point.id);
            blake3::hash(key.as_bytes()).to_hex().to_string()
        });
        let count = eligible.len().div_ceil(5);
        return eligible
            .into_iter()
            .take(count)
            .filter(|(_, point)| point.second_label.is_none())
            .map(|(index, _)| index)
            .collect();
    }
    project
        .points
        .iter()
        .enumerate()
        .filter(|(_, point)| requested.map_or(point.label.is_none(), |id| point.id == id))
        .map(|(index, _)| index)
        .collect()
}

pub(super) fn code_excerpt(checkout: &Path, location: &str) -> io::Result<String> {
    let (name, line) = location
        .rsplit_once(':')
        .ok_or_else(|| io::Error::other("invalid decision location"))?;
    let line: usize = line.parse().map_err(io::Error::other)?;
    let source = fetch::git(checkout, &["show", &format!("HEAD:{name}")], false)?;
    let mut result = String::new();
    for (index, value) in source
        .lines()
        .enumerate()
        .skip(line.saturating_sub(8))
        .take(18)
    {
        let prefix = if index + 1 == line { ">" } else { " " };
        writeln!(result, "{prefix} {:>4} {value}", index + 1).map_err(io::Error::other)?;
    }
    Ok(result)
}

fn show(output: &mut impl Write, point: &Point, code: &str, second: bool) -> io::Result<()> {
    writeln!(output, "{} {}\n{code}", point.id, point.location)?;
    for (index, evidence) in point.evidence.iter().enumerate() {
        writeln!(
            output,
            "{}. {:?} {} {}",
            index + 1,
            evidence.kind,
            evidence.url,
            evidence.quote.as_deref().unwrap_or_default()
        )?;
    }
    if second {
        writeln!(
            output,
            "e <kind> <url> <quote> | add evidence for this second review"
        )?;
    } else {
        if let Some(draft) = &point.draft {
            writeln!(output, "Draft {:?}: {}", draft.label, draft.reason)?;
        }
        writeln!(
            output,
            "a accept | c <category> | e <kind> <url> <quote> | x <evidence number> | n <note>"
        )?;
    }
    writeln!(
        output,
        "s significant | t trivial | u unclear | k skip | q quit"
    )?;
    output.flush()
}

enum Progress {
    Save,
    Skip,
    Quit,
}

fn edit_point(
    input: &mut impl BufRead,
    output: &mut impl Write,
    point: &mut Point,
    reviewer: &str,
    second: bool,
) -> io::Result<Progress> {
    let mut independent_evidence = Vec::new();
    loop {
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(Progress::Quit);
        }
        let line = line.trim();
        let (action, argument) = line.split_once(' ').unwrap_or((line, ""));
        let draft = point.draft.as_ref().filter(|_| !second);
        let label = match action {
            "a" => draft.map(|draft| draft.label),
            "s" => Some(Label::Significant),
            "t" => Some(Label::Trivial),
            "u" => Some(Label::Unclear),
            "k" => return Ok(Progress::Skip),
            "q" => return Ok(Progress::Quit),
            _ => None,
        };
        if let Some(label) = label {
            let category = if action == "a" {
                draft.and_then(|draft| draft.category)
            } else {
                point.category
            };
            if label_allowed(
                point.evidence.iter().chain(&independent_evidence),
                label,
                category,
                second,
            ) {
                if second {
                    point.second_label = Some(label);
                    point.second_by = Some(reviewer.to_owned());
                } else {
                    point.label = Some(label);
                    point.category = category;
                    point.verified_by = Some(reviewer.to_owned());
                }
                return Ok(Progress::Save);
            }
            writeln!(
                output,
                "Significant needs documented evidence. Trivial needs a category. Use u if unclear."
            )?;
        } else if second && action == "e" && add_evidence(&mut independent_evidence, argument)
            || !second && edit_metadata(point, action, argument)
        {
            continue;
        } else {
            writeln!(output, "Choose an available action.")?;
        }
        output.flush()?;
    }
}

fn edit_metadata(point: &mut Point, action: &str, argument: &str) -> bool {
    match action {
        "e" => add_evidence(&mut point.evidence, argument),
        "c" => {
            point.category =
                serde_json::from_value(serde_json::Value::String(argument.to_owned())).ok();
            point.category.is_some()
        }
        "x" => {
            if let Some(index) = argument
                .parse::<usize>()
                .ok()
                .and_then(|index| index.checked_sub(1))
                && index < point.evidence.len()
            {
                point.evidence.remove(index);
                return true;
            }
            false
        }
        "n" if !argument.is_empty() => {
            point.note = Some(argument.to_owned());
            true
        }
        _ => false,
    }
}

fn add_evidence(evidence: &mut Vec<Evidence>, argument: &str) -> bool {
    let mut fields = argument.splitn(3, ' ');
    let Some(kind) = fields.next().and_then(|name| {
        serde_json::from_value::<EvidenceKind>(serde_json::Value::String(name.to_owned())).ok()
    }) else {
        return false;
    };
    let Some(url) = fields.next().filter(|url| !url.is_empty()) else {
        return false;
    };
    let Some(quote) = fields.next().filter(|quote| !quote.trim().is_empty()) else {
        return false;
    };
    evidence.push(Evidence {
        kind,
        url: url.to_owned(),
        quote: Some(quote.to_owned()),
    });
    true
}

fn label_allowed<'a>(
    mut evidence: impl Iterator<Item = &'a Evidence>,
    label: Label,
    category: Option<Category>,
    second: bool,
) -> bool {
    let kinds = [EvidenceKind::Docs, EvidenceKind::Spec, EvidenceKind::Issue];
    match label {
        Label::Significant => evidence.any(|item| kinds.contains(&item.kind)),
        Label::Trivial => second || category.is_some(),
        Label::Unclear => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::sample::{
        self,
        tests::{points, project},
    };

    #[test]
    fn second_review_hides_draft_and_first_label() {
        let mut project = project();
        sample::freeze(&mut project, &points()).unwrap();
        let point = &mut project.points[0];
        point.verified_by = Some("first".to_owned());
        point.label = Some(Label::Significant);
        point.draft = Some(crate::corpus::manifest::Draft {
            label: Label::Significant,
            category: None,
            evidence: Vec::new(),
            reason: "secret draft".to_owned(),
        });
        let mut buffer = Vec::new();
        show(&mut buffer, point, "code", true).unwrap();
        let output = String::from_utf8(buffer).unwrap();
        assert!(!output.contains("secret draft"));
        assert!(!output.contains("Significant"));
        assert!(matches!(
            edit_point(&mut &b"t\n"[..], &mut Vec::new(), point, "second", true).unwrap(),
            Progress::Save
        ));
        assert_eq!(point.label, Some(Label::Significant));
        assert_eq!(point.second_label, Some(Label::Trivial));
    }

    #[test]
    fn second_review_accepts_independent_evidence_without_changing_first_review() {
        let mut project = project();
        sample::freeze(&mut project, &points()).unwrap();
        let point = &mut project.points[0];
        point.label = Some(Label::Unclear);
        let input = b"s\ne test https://example.org/test Test result\ns\ne docs https://example.org/rule Rule text\ns\n";
        let mut output = Vec::new();

        assert!(matches!(
            edit_point(&mut &input[..], &mut output, point, "second", true).unwrap(),
            Progress::Save
        ));
        assert_eq!(
            String::from_utf8(output)
                .unwrap()
                .matches("Significant needs")
                .count(),
            2
        );
        assert_eq!(point.label, Some(Label::Unclear));
        assert!(point.evidence.is_empty());
        assert_eq!(point.second_label, Some(Label::Significant));
    }
}
