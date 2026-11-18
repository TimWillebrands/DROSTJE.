use serde::{Deserialize, Serialize};

use crate::ToolOutcome;

const FOLDS_FROM: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunPart {
    Break,
    Transparent,
    Opens(usize),
    Calls(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolRun {
    pub head: usize,
    pub end: usize,
    pub calls: usize,
}

impl ToolRun {
    pub fn folds(self) -> bool {
        self.calls >= FOLDS_FROM
    }
}

pub fn tool_run(part: impl Fn(usize) -> RunPart, len: usize, index: usize) -> Option<ToolRun> {
    let mut head = None;
    for ix in (0..=index).rev() {
        match part(ix) {
            RunPart::Break => break,
            RunPart::Opens(_) => {
                head = Some(ix);
                break;
            }
            RunPart::Calls(_) => head = Some(ix),
            RunPart::Transparent => {}
        }
    }
    let head = head?;
    let (RunPart::Opens(mut calls) | RunPart::Calls(mut calls)) = part(head) else {
        return None;
    };
    let mut end = head + 1;
    while end < len {
        match part(end) {
            RunPart::Calls(more) => calls += more,
            RunPart::Transparent => {}
            RunPart::Break | RunPart::Opens(_) => break,
        }
        end += 1;
    }
    Some(ToolRun { head, end, calls })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunTally {
    pub calls: usize,
    pub running: usize,
    pub failed: usize,
    pub unknown: usize,
}

impl RunTally {
    pub fn add(&mut self, outcome: ToolOutcome) {
        self.calls += 1;
        match outcome {
            ToolOutcome::Running => self.running += 1,
            ToolOutcome::Failed => self.failed += 1,
            ToolOutcome::Unknown => self.unknown += 1,
            ToolOutcome::Succeeded => {}
        }
    }

    pub fn folds(&self) -> bool {
        self.calls >= FOLDS_FROM
    }

    pub fn outcome(&self) -> ToolOutcome {
        if self.running > 0 {
            ToolOutcome::Running
        } else if self.failed > 0 {
            ToolOutcome::Failed
        } else if self.unknown > 0 {
            ToolOutcome::Unknown
        } else {
            ToolOutcome::Succeeded
        }
    }

    pub fn title(&self) -> String {
        match self.calls {
            1 => "1 tool call".to_owned(),
            calls => format!("{calls} tool calls"),
        }
    }

    pub fn without_result(&self) -> Option<String> {
        match self.unknown {
            0 => None,
            1 => Some("1 without result".to_owned()),
            unknown => Some(format!("{unknown} without result")),
        }
    }

    pub fn failures(&self) -> Option<String> {
        (self.failed > 0).then(|| format!("{} failed", self.failed))
    }
}

impl FromIterator<ToolOutcome> for RunTally {
    fn from_iter<T: IntoIterator<Item = ToolOutcome>>(outcomes: T) -> Self {
        let mut tally = Self::default();
        for outcome in outcomes {
            tally.add(outcome);
        }
        tally
    }
}

#[cfg(test)]
mod tests {
    use super::{RunPart, RunTally, ToolRun, tool_run};
    use crate::ToolOutcome;

    #[test]
    fn runs_span_calls_across_messages_and_skip_hidden_rows() {
        use RunPart::{Break, Calls, Opens, Transparent};
        let parts = [
            Break,
            Opens(1),
            Transparent,
            Calls(1),
            Transparent,
            Calls(2),
            Break,
            Calls(1),
            Opens(1),
            Calls(1),
        ];
        let run = |index| tool_run(|ix| parts[ix], parts.len(), index);
        let first = Some(ToolRun {
            head: 1,
            end: 6,
            calls: 4,
        });
        assert_eq!(run(0), None);
        for index in 1..6 {
            assert_eq!(run(index), first, "row {index}");
        }
        assert_eq!(run(6), None);
        let lone = run(7).unwrap();
        assert_eq!((lone.head, lone.end, lone.folds()), (7, 8, false));
        assert_eq!(
            run(9),
            Some(ToolRun {
                head: 8,
                end: 10,
                calls: 2
            })
        );
        assert_eq!(tool_run(|ix| [Break, Transparent][ix], 2, 1), None);
    }

    #[test]
    fn a_tally_counts_outcomes_and_says_what_needs_attention() {
        let tally: RunTally = [
            ToolOutcome::Succeeded,
            ToolOutcome::Failed,
            ToolOutcome::Unknown,
            ToolOutcome::Unknown,
        ]
        .into_iter()
        .collect();
        assert_eq!(tally.title(), "4 tool calls");
        assert_eq!(tally.failures().as_deref(), Some("1 failed"));
        assert_eq!(tally.without_result().as_deref(), Some("2 without result"));
        assert_eq!(tally.outcome(), ToolOutcome::Failed);
        assert!(tally.folds());
        let lone: RunTally = [ToolOutcome::Running].into_iter().collect();
        assert_eq!(lone.title(), "1 tool call");
        assert_eq!((lone.failures(), lone.without_result()), (None, None));
        assert_eq!(lone.outcome(), ToolOutcome::Running);
        assert!(!lone.folds());
        let unknown: RunTally = [ToolOutcome::Succeeded, ToolOutcome::Unknown]
            .into_iter()
            .collect();
        assert_eq!(
            unknown.without_result().as_deref(),
            Some("1 without result")
        );
        assert_eq!(unknown.outcome(), ToolOutcome::Unknown);
    }
}
