use std::path::Path;

use super::repository::{FileTimestamp, UnixMetadata};
use crate::jobs::JobError;
use chrono::{DateTime, Utc};
use http::StatusCode;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct Filter {
    relative_path: RegexFilter,
    uid: RegexFilter,
    gid: RegexFilter,
    mode: RegexFilter,
    atime: TimeFilter,
    mtime: TimeFilter,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct RegexFilter {
    include: Vec<String>,
    exclude: Vec<String>,
}

// include from, exclude before
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TimeFilter {
    from: Option<DateTime<Utc>>,
    before: Option<DateTime<Utc>>,
}

impl Filter {
    pub(super) fn compile(&self) -> Result<CompiledFilter, JobError> {
        self.atime.validate()?;
        self.mtime.validate()?;

        Ok(CompiledFilter {
            relative_path: self.relative_path.compile()?,
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
                    regex::Regex::new(pattern).map_err(|error| {
                        JobError::new(
                            StatusCode::BAD_REQUEST,
                            format!("failed to parse include regex {pattern}: {error}"),
                        )
                    })
                })
                .collect::<Result<_, _>>()?,
            exclude: self
                .exclude
                .iter()
                .map(|pattern| {
                    regex::Regex::new(pattern).map_err(|error| {
                        JobError::new(
                            StatusCode::BAD_REQUEST,
                            format!("failed to parse exclude regex {pattern}: {error}"),
                        )
                    })
                })
                .collect::<Result<_, _>>()?,
        })
    }
}

impl TimeFilter {
    fn validate(&self) -> Result<(), JobError> {
        if let (Some(from), Some(before)) = (self.from, self.before)
            && from >= before
        {
            return Err(JobError::new(
                StatusCode::BAD_REQUEST,
                "time filter requires from < before".to_string(),
            ));
        }
        Ok(())
    }

    fn matches(&self, timestamp: &FileTimestamp) -> Result<bool, JobError> {
        let time = DateTime::<Utc>::from_timestamp(timestamp.seconds, timestamp.nanoseconds)
            .ok_or_else(|| JobError::internal("timestamp is out of range"))?;

        Ok(self.from.as_ref().is_none_or(|from| &time >= from)
            && self.before.as_ref().is_none_or(|before| &time < before))
    }
}

pub(super) struct CompiledFilter {
    relative_path: CompiledRegexFilter,
    uid: CompiledRegexFilter,
    gid: CompiledRegexFilter,
    mode: CompiledRegexFilter,
    atime: TimeFilter,
    mtime: TimeFilter,
}

impl CompiledFilter {
    pub(super) fn matches(
        &self,
        relative_path: &Path,
        metadata: &UnixMetadata,
    ) -> Result<bool, JobError> {
        Ok(self.relative_path.matches(
            relative_path
                .to_str()
                .ok_or_else(|| JobError::new(StatusCode::BAD_REQUEST, "non UTF-8 path"))?,
        ) && self.uid.matches(&metadata.uid.to_string())
            && self.gid.matches(&metadata.gid.to_string())
            && self.mode.matches(&format!("{:04o}", metadata.mode))
            && self.atime.matches(&metadata.atime)?
            && self.mtime.matches(&metadata.mtime)?)
    }
}

struct CompiledRegexFilter {
    include: Vec<regex::Regex>,
    exclude: Vec<regex::Regex>,
}

impl CompiledRegexFilter {
    fn matches(&self, value: &str) -> bool {
        let included = self.include.is_empty() || self.include.iter().any(|r| r.is_match(value));

        let excluded = self.exclude.iter().any(|r| r.is_match(value));
        included && !excluded
    }
}
