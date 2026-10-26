use crate::jobs::JobError;
use chrono::{DateTime, Utc};
use http::StatusCode;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct Filter {
    pub(super) name: RegexFilter,
    pub(super) uid: RegexFilter,
    pub(super) gid: RegexFilter,
    pub(super) mode: RegexFilter,
    pub(super) atime: TimeFilter,
    pub(super) mtime: TimeFilter,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct RegexFilter {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

// include from, exclude before
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TimeFilter {
    pub(super) from: Option<DateTime<Utc>>,
    pub(super) before: Option<DateTime<Utc>>,
}

impl Filter {
    pub(super) fn compile(&self) -> Result<CompiledFilter, JobError> {
        self.atime.validate()?;
        self.mtime.validate()?;

        Ok(CompiledFilter {
            name: self.name.compile()?,
            uid: self.uid.compile()?,
            gid: self.gid.compile()?,
            mode: self.mode.compile()?,
            atime: self.atime.clone(),
            mtime: self.mtime.clone(),
        })
    }
}

impl RegexFilter {
    fn compile(&self) -> Result<CompiledRegexFilter, JobError> {
        Ok(CompiledRegexFilter {
            include: self
                .include
                .iter()
                .map(|pattern| {
                    regex::Regex::new(pattern).map_err(|error| JobError {
                        code: StatusCode::BAD_REQUEST,
                        message: format!("failed to parse include regex {pattern}: {error}"),
                    })
                })
                .collect::<Result<_, _>>()?,
            exclude: self
                .exclude
                .iter()
                .map(|pattern| {
                    regex::Regex::new(pattern).map_err(|error| JobError {
                        code: StatusCode::BAD_REQUEST,
                        message: format!("failed to parse exclude regex {pattern}: {error}"),
                    })
                })
                .collect::<Result<_, _>>()?,
        })
    }
}

impl TimeFilter {
    fn validate(&self) -> Result<(), JobError> {
        if let (Some(from), Some(before)) = (self.from, self.before) && from >= before {
            return Err(JobError {
                code: StatusCode::BAD_REQUEST,
                message: "time filter requires from < before".to_string(),
            })
        }
        Ok(())
    }
}

struct CompiledFilter {
    name: CompiledRegexFilter,
    uid: CompiledRegexFilter,
    gid: CompiledRegexFilter,
    mode: CompiledRegexFilter,
    atime: TimeFilter,
    mtime: TimeFilter,
}

struct CompiledRegexFilter {
    include: Vec<regex::Regex>,
    exclude: Vec<regex::Regex>,
}
