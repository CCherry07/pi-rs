//! Shared Pi v4 wire values; storage and session orchestration live in pi-session.
mod wire;
pub use wire::*;
#[doc(hidden)]
pub mod strict_optional {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
    {
        T::deserialize(deserializer).map(Some)
    }
}

#[doc(hidden)]
pub mod required_nullable {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
    {
        Option::<T>::deserialize(deserializer)
    }
}

#[doc(hidden)]
pub mod iso_timestamp_ms {
    use std::fmt;

    use serde::de::Visitor;
    use serde::{Deserializer, Serializer};
    use time::format_description::well_known::Rfc3339;
    use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time};

    pub fn serialize<S>(timestamp_ms: &i64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let timestamp = OffsetDateTime::from_unix_timestamp_nanos(
            i128::from(*timestamp_ms).saturating_mul(1_000_000),
        )
        .map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(
            &timestamp
                .format(&Rfc3339)
                .map_err(serde::ser::Error::custom)?,
        )
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<i64, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct TimestampVisitor;

        impl Visitor<'_> for TimestampVisitor {
            type Value = i64;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an integer millisecond timestamp or RFC3339 string")
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(value)
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                i64::try_from(value).map_err(E::custom)
            }

            fn visit_borrowed_str<E>(self, value: &'_ str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                parse_rfc3339_ms(value).map_err(E::custom)
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                parse_rfc3339_ms(value).map_err(E::custom)
            }
        }

        deserializer.deserialize_any(TimestampVisitor)
    }

    fn parse_rfc3339_ms(value: &str) -> Result<i64, String> {
        if let Some(timestamp) = parse_canonical_utc_ms(value) {
            return Ok(timestamp);
        }
        let timestamp =
            OffsetDateTime::parse(value, &Rfc3339).map_err(|error| error.to_string())?;
        i64::try_from(timestamp.unix_timestamp_nanos() / 1_000_000)
            .map_err(|error| error.to_string())
    }

    fn parse_canonical_utc_ms(value: &str) -> Option<i64> {
        let bytes = value.as_bytes();
        let fractional = match bytes.len() {
            20 if bytes[19] == b'Z' => 0,
            24 if bytes[19] == b'.' && bytes[23] == b'Z' => decimal(bytes, 20, 3)?,
            _ => return None,
        };
        if bytes[4] != b'-'
            || bytes[7] != b'-'
            || bytes[10] != b'T'
            || bytes[13] != b':'
            || bytes[16] != b':'
        {
            return None;
        }
        let year = i32::try_from(decimal(bytes, 0, 4)?).ok()?;
        let month = Month::try_from(u8::try_from(decimal(bytes, 5, 2)?).ok()?).ok()?;
        let day = u8::try_from(decimal(bytes, 8, 2)?).ok()?;
        let hour = u8::try_from(decimal(bytes, 11, 2)?).ok()?;
        let minute = u8::try_from(decimal(bytes, 14, 2)?).ok()?;
        let second = u8::try_from(decimal(bytes, 17, 2)?).ok()?;
        let millisecond = u16::try_from(fractional).ok()?;
        let date = Date::from_calendar_date(year, month, day).ok()?;
        let time = Time::from_hms_milli(hour, minute, second, millisecond).ok()?;
        let timestamp = PrimitiveDateTime::new(date, time).assume_utc();
        i64::try_from(timestamp.unix_timestamp_nanos() / 1_000_000).ok()
    }

    fn decimal(bytes: &[u8], start: usize, len: usize) -> Option<u32> {
        bytes
            .get(start..start.checked_add(len)?)?
            .iter()
            .try_fold(0_u32, |value, byte| {
                byte.is_ascii_digit().then(|| {
                    value
                        .saturating_mul(10)
                        .saturating_add(u32::from(*byte - b'0'))
                })
            })
    }
}
