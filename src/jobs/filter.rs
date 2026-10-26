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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{unfiltered, unfiltered_config};
    use serde_json::json;
    use std::error::Error;

    type TestResult = Result<(), Box<dyn Error>>;

    fn metadata() -> UnixMetadata {
        UnixMetadata {
            uid: 1000,
            gid: 2000,
            mode: 0o644,
            atime: FileTimestamp {
                seconds: 100,
                nanoseconds: 150,
            },
            mtime: FileTimestamp {
                seconds: 100,
                nanoseconds: 150,
            },
        }
    }

    #[test]
    fn empty_rules_allow_entries() -> TestResult {
        assert!(
            unfiltered()?
                .compile()?
                .matches(Path::new("目录/report.txt"), &metadata())?
        );
        Ok(())
    }

    #[test]
    fn regex_includes_are_alternatives_and_excludes_take_precedence() -> TestResult {
        let mut config = unfiltered_config();
        config["relative_path"] = json!({
            "include": ["\\.txt$", "^docs/"],
            "exclude": ["^docs/private(/|$)"]
        });
        let filter: Filter = serde_json::from_value(config)?;
        let filter = filter.compile()?;
        for (path, expected) in [
            ("report.txt", true),
            ("docs/report.csv", true),
            ("docs/private/report.txt", false),
            ("image.png", false),
        ] {
            assert_eq!(
                filter.matches(Path::new(path), &metadata())?,
                expected,
                "{path}"
            );
        }
        Ok(())
    }

    #[test]
    fn metadata_fields_are_combined_and_mode_is_octal() -> TestResult {
        let mut config = unfiltered_config();
        config["relative_path"]["include"] = json!(["^docs/"]);
        config["uid"]["include"] = json!(["^999$", "^1000$"]);
        config["gid"]["include"] = json!(["^2000$"]);
        config["mode"]["include"] = json!(["^0644$"]);
        let filter: Filter = serde_json::from_value(config.clone())?;
        assert!(
            filter
                .compile()?
                .matches(Path::new("docs/report.txt"), &metadata())?
        );
        assert!(
            !filter
                .compile()?
                .matches(Path::new("other/report.txt"), &metadata())?
        );
        for (field, pattern) in [("uid", "^1000$"), ("gid", "^2000$"), ("mode", "^0644$")] {
            let mut excluded = config.clone();
            excluded[field]["exclude"] = json!([pattern]);
            let filter: Filter = serde_json::from_value(excluded)?;
            assert!(
                !filter
                    .compile()?
                    .matches(Path::new("docs/report.txt"), &metadata())?,
                "{field}"
            );
        }
        config["mode"]["include"] = json!(["^420$"]);
        let filter: Filter = serde_json::from_value(config)?;
        assert!(
            !filter
                .compile()?
                .matches(Path::new("docs/report.txt"), &metadata())?
        );
        Ok(())
    }

    #[test]
    fn time_ranges_use_inclusive_from_and_exclusive_before_at_nanosecond_precision() -> TestResult {
        for field in ["atime", "mtime"] {
            let mut config = unfiltered_config();
            config[field] = json!({
                "from": "1970-01-01T01:01:40.000000100+01:00",
                "before": "1970-01-01T00:01:40.000000200Z"
            });
            let filter: Filter = serde_json::from_value(config)?;
            let filter = filter.compile()?;
            for (nanoseconds, expected) in [(99, false), (100, true), (199, true), (200, false)] {
                let mut metadata = metadata();
                let timestamp = FileTimestamp {
                    seconds: 100,
                    nanoseconds,
                };
                if field == "atime" {
                    metadata.atime = timestamp;
                } else {
                    metadata.mtime = timestamp;
                }
                assert_eq!(
                    filter.matches(Path::new("file"), &metadata)?,
                    expected,
                    "{field}: {nanoseconds}"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn time_ranges_allow_either_bound_to_be_omitted() -> TestResult {
        for field in ["atime", "mtime"] {
            for (range, expected) in [
                (json!({}), true),
                (json!({"from": "1970-01-01T00:01:40Z"}), true),
                (json!({"from": "1970-01-01T00:01:41Z"}), false),
                (json!({"before": "1970-01-01T00:01:41Z"}), true),
                (json!({"before": "1970-01-01T00:01:40Z"}), false),
            ] {
                let mut config = unfiltered_config();
                config[field] = range;
                let filter: Filter = serde_json::from_value(config)?;
                assert_eq!(
                    filter.compile()?.matches(Path::new("file"), &metadata())?,
                    expected
                );
            }
        }
        Ok(())
    }

    #[test]
    fn invalid_regexes_and_time_ranges_are_rejected() -> TestResult {
        for field in ["relative_path", "uid", "gid", "mode"] {
            for list in ["include", "exclude"] {
                let mut config = unfiltered_config();
                config[field][list] = json!(["["]);
                let filter: Filter = serde_json::from_value(config)?;
                let error = filter.compile().err().ok_or("accepted invalid regex")?;
                assert_eq!(error.code, StatusCode::BAD_REQUEST);
            }
        }
        for field in ["atime", "mtime"] {
            for before in ["1970-01-01T00:00:00Z", "1969-12-31T23:59:59Z"] {
                let mut config = unfiltered_config();
                config[field] = json!({"from": "1970-01-01T00:00:00Z", "before": before});
                let filter: Filter = serde_json::from_value(config)?;
                assert_eq!(
                    filter.compile().err().ok_or("accepted invalid range")?.code,
                    StatusCode::BAD_REQUEST
                );
            }
        }
        Ok(())
    }
}
