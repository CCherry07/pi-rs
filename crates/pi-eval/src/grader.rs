use crate::{EvalGrade, EvalObservation};

pub trait EvalGrader: Send + Sync {
    fn name(&self) -> &str;
    fn grade(&self, observation: &EvalObservation) -> EvalGrade;
}

#[derive(Debug, Clone)]
pub struct ExactOutputGrader {
    expected: String,
    trim: bool,
    required: bool,
}

impl ExactOutputGrader {
    pub fn new(expected: impl Into<String>) -> Self {
        Self {
            expected: expected.into(),
            trim: true,
            required: true,
        }
    }

    pub fn trim(mut self, trim: bool) -> Self {
        self.trim = trim;
        self
    }

    pub fn required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }
}

impl EvalGrader for ExactOutputGrader {
    fn name(&self) -> &str {
        "exact_output"
    }

    fn grade(&self, observation: &EvalObservation) -> EvalGrade {
        let actual = if self.trim {
            observation.final_response.trim()
        } else {
            &observation.final_response
        };
        let expected = if self.trim {
            self.expected.trim()
        } else {
            &self.expected
        };
        let passed = actual == expected;
        EvalGrade {
            grader: self.name().to_string(),
            score: if passed { 1.0 } else { 0.0 },
            passed,
            required: self.required,
            rationale: if passed {
                "final response matched the expected text".to_string()
            } else {
                format!("expected {expected:?}, received {actual:?}")
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EvalUsage;

    fn observation(response: &str) -> EvalObservation {
        EvalObservation {
            system_prompt: None,
            final_response: response.to_string(),
            transcript: Vec::new(),
            workspace_changes: Vec::new(),
            usage: EvalUsage::default(),
            errors: Vec::new(),
        }
    }

    #[test]
    fn exact_output_trims_by_default_and_explains_failure() {
        let grader = ExactOutputGrader::new("Paris");
        assert!(grader.grade(&observation(" Paris\n")).passed);
        let grade = grader.grade(&observation("Lyon"));
        assert!(!grade.passed);
        assert!(grade.rationale.contains("Lyon"));
    }
}
