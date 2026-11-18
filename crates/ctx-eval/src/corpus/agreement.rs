use serde::Serialize;

use super::manifest::{Label, Project};

#[derive(Serialize)]
pub(super) struct Agreement {
    pub n: usize,
    pub observed: Option<f64>,
    pub expected: Option<f64>,
    pub kappa: Option<f64>,
    pub matrix: [[usize; 3]; 3],
    pub disagreements: Vec<Disagreement>,
}

#[derive(Serialize)]
pub(super) struct Disagreement {
    id: String,
    location: String,
    first: Label,
    second: Label,
}

pub(super) fn agreement(project: &Project) -> Agreement {
    let mut matrix = [[0; 3]; 3];
    let mut disagreements = Vec::new();
    for point in &project.points {
        if let (Some(first), Some(second)) = (point.label, point.second_label) {
            matrix[label_index(first)][label_index(second)] += 1;
            if first != second {
                disagreements.push(Disagreement {
                    id: point.id.clone(),
                    location: point.location.clone(),
                    first,
                    second,
                });
            }
        }
    }
    let n = matrix.iter().flatten().sum();
    let (observed, expected, kappa) = statistics(matrix, n);
    Agreement {
        n,
        observed,
        expected,
        kappa,
        matrix,
        disagreements,
    }
}

#[allow(clippy::cast_precision_loss)]
fn statistics(matrix: [[usize; 3]; 3], count: usize) -> (Option<f64>, Option<f64>, Option<f64>) {
    if count == 0 {
        return (None, None, None);
    }
    let n = count as f64;
    let observed = (0..3).map(|i| matrix[i][i]).sum::<usize>() as f64 / n;
    let expected = (0..3)
        .map(|i| {
            let row = matrix[i].iter().sum::<usize>() as f64;
            let column = matrix.iter().map(|row| row[i]).sum::<usize>() as f64;
            (row / n) * (column / n)
        })
        .sum::<f64>();
    let kappa = (expected < 1.0).then(|| (observed - expected) / (1.0 - expected));
    (Some(observed), Some(expected), kappa)
}

const fn label_index(label: Label) -> usize {
    match label {
        Label::Significant => 0,
        Label::Trivial => 1,
        Label::Unclear => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_kappa_and_reports_undefined_agreement() {
        assert_eq!(
            statistics([[3, 1, 0], [1, 3, 0], [0, 0, 0]], 8),
            (Some(0.75), Some(0.5), Some(0.5))
        );
        assert_eq!(
            statistics([[2, 0, 0], [0, 2, 0], [0, 0, 0]], 4),
            (Some(1.0), Some(0.5), Some(1.0))
        );
        assert_eq!(
            statistics([[4, 0, 0], [0, 0, 0], [0, 0, 0]], 4),
            (Some(1.0), Some(1.0), None)
        );
    }
}
