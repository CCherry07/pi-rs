use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use croner::Cron;
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Persist absolute one-shots so reopening never restarts their relative delay.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Schedule {
    Once {
        at: i64,
    },
    Interval {
        milliseconds: i64,
    },
    Cron {
        expression: String,
        timezone: String,
    },
}

impl Schedule {
    pub fn parse(input: &str, timezone: &str, now: i64) -> Result<Self> {
        let input = input.trim();
        let _: Tz = timezone
            .parse()
            .map_err(|_| Error::Invalid("invalid IANA timezone".into()))?;
        let schedule = if let Some(delay) = input.strip_prefix("in ") {
            Self::Once {
                at: now
                    .checked_add(duration_ms(delay)?)
                    .ok_or_else(|| Error::Invalid("date overflow".into()))?,
            }
        } else if let Some(interval) = input.strip_prefix("every ") {
            Self::Interval {
                milliseconds: duration_ms(interval)?,
            }
        } else if let Ok(at) = DateTime::parse_from_rfc3339(input) {
            if at.timestamp_millis() <= now {
                return Err(Error::Invalid(
                    "one-shot timestamp must be in the future".into(),
                ));
            }
            Self::Once {
                at: at.timestamp_millis(),
            }
        } else {
            if input.split_whitespace().count() != 5 {
                return Err(Error::Invalid("use 'in 30m', 'every 2h', an RFC3339 timestamp, or a five-field cron expression".into()));
            }
            Self::Cron {
                expression: input.into(),
                timezone: timezone.into(),
            }
        };
        schedule.next(now)?;
        Ok(schedule)
    }

    pub fn next(&self, now: i64) -> Result<i64> {
        match self {
            Self::Once { at } => Ok(*at),
            Self::Interval { milliseconds } if *milliseconds >= 1000 => now
                .checked_add(*milliseconds)
                .ok_or_else(|| Error::Invalid("interval overflow".into())),
            Self::Interval { .. } => Err(Error::Invalid(
                "interval must be at least one second".into(),
            )),
            Self::Cron {
                expression,
                timezone,
            } => {
                let cron: Cron = expression
                    .parse()
                    .map_err(|error| Error::Invalid(format!("invalid cron: {error}")))?;
                let timezone: Tz = timezone
                    .parse()
                    .map_err(|_| Error::Invalid("invalid IANA timezone".into()))?;
                let now = DateTime::<Utc>::from_timestamp_millis(now)
                    .ok_or_else(|| Error::Invalid("invalid timestamp".into()))?
                    .with_timezone(&timezone);
                cron.find_next_occurrence(&now, false)
                    .map(|at| at.timestamp_millis())
                    .map_err(|error| {
                        Error::Invalid(format!("cron has no next occurrence: {error}"))
                    })
            }
        }
    }

    pub fn recurring(&self) -> bool {
        !matches!(self, Self::Once { .. })
    }
}

fn duration_ms(input: &str) -> Result<i64> {
    let input = input.trim();
    let (number, multiplier) = match input.as_bytes().last() {
        Some(b's') => (&input[..input.len() - 1], 1000_i64),
        Some(b'm') => (&input[..input.len() - 1], 60_000),
        Some(b'h') => (&input[..input.len() - 1], 3_600_000),
        Some(b'd') => (&input[..input.len() - 1], 86_400_000),
        _ => {
            return Err(Error::Invalid(
                "duration needs a positive integer and s/m/h/d suffix".into(),
            ));
        }
    };
    number
        .trim()
        .parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
        .and_then(|n| n.checked_mul(multiplier))
        .ok_or_else(|| Error::Invalid("invalid or overflowing duration".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_times_and_cron_timezone_are_unambiguous() {
        let now = DateTime::parse_from_rfc3339("2026-09-08T00:00:00Z")
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            Schedule::parse("in 30m", "UTC", now)
                .unwrap()
                .next(now)
                .unwrap(),
            now + 1_800_000
        );
        assert_eq!(
            Schedule::parse("every 2h", "UTC", now)
                .unwrap()
                .next(now)
                .unwrap(),
            now + 7_200_000
        );
        assert_eq!(
            Schedule::parse("0 9 * * *", "Asia/Shanghai", now)
                .unwrap()
                .next(now)
                .unwrap(),
            now + 3_600_000
        );
        for input in [
            "in 0m",
            "every -1h",
            "every 999999999999999999d",
            "* * *",
            "2020-01-01T00:00:00Z",
        ] {
            assert!(Schedule::parse(input, "UTC", now).is_err(), "{input}");
        }
        assert!(Schedule::parse("0 9 * * *", "nonsense", now).is_err());
    }
}
