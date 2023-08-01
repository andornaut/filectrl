use chrono::{DateTime, Datelike, Local, Timelike};
use std::cmp;

const FACTOR: u64 = 1024;
const UNITS: [&str; 6] = ["", "K", "M", "G", "T", "P"];

pub(super) fn humanize_bytes(bytes: u64) -> String {
    if bytes == 0 {
        // Avoid panic: "argument of integer logarithm must be positive"
        return "0".to_string();
    }
    let mut floor = bytes.ilog10() / FACTOR.ilog10();
    floor = cmp::min(floor, (UNITS.len() - 1) as u32);
    let rounded = ((bytes as f64) / (FACTOR.pow(floor) as f64)).round();
    let unit = UNITS[floor as usize];
    format!("{rounded}{unit}")
}

pub(super) fn humanize_datetime(
    datetime: DateTime<Local>,
    relative_to_datetime: DateTime<Local>,
) -> String {
    let naive_relative_to_date = relative_to_datetime
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let naive_date = datetime.date_naive().and_hms_opt(0, 0, 0).unwrap();
    let format = if naive_date == naive_relative_to_date {
        // Formats: https://docs.rs/chrono/latest/chrono/format/strftime/index.html
        if datetime.hour() == relative_to_datetime.hour()
            && datetime.minute() == relative_to_datetime.minute()
        {
            "%I:%M:%S%P"
        } else {
            "%I:%M%P"
        }
    } else if naive_date.year() == naive_relative_to_date.year() {
        "%b %d"
    } else {
        "%b %d, %Y"
    };
    // Return eg. "6:00am" instead of "06:00am"
    let mut datetime = format!("{}", datetime.format(format));
    if datetime.starts_with('0') {
        datetime.remove(0);
    }
    datetime
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDateTime, TimeZone};
    use test_case::test_case;

    const DATETIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

    #[test_case("0",  0u64 ; "zero bytes")]
    #[test_case("499",  499u64 ; "between 1 and 999 bytes")]
    #[test_case("10M",  10_000_000u64 ; "10 million bytes rounds up")]
    #[test_case("477M",  500 * 1000u64.pow(2) ; "500 million bytes rounds down")]
    #[test_case("500M",  500 * 1024u64.pow(2) ; "500 MB doesn't round")]
    #[test_case("1G",  1000_000_000u64 ; "1 billion bytes rounds up")]
    #[test_case("1P",  1024u64.pow(5); "max unit")]
    #[test_case("1096P",  1234_000_000_000_000_000u64; "greater than max unit")]
    fn humanize_bytes_is_correct(expected: &str, bytes: u64) {
        let result = humanize_bytes(bytes);

        assert_eq!(expected, result);
    }

    #[test_case("6:00:00am", "2023-07-12 6:00:00", "2023-07-12 6:00:00"; "same time, strip leading 0")]
    #[test_case("12:30:10pm", "2023-07-12 12:30:10", "2023-07-12 12:30:20"; "different second")]
    #[test_case("12:30pm", "2023-07-12 12:30:10", "2023-07-12 12:31:10"; "different minute")]
    #[test_case("12:30pm", "2023-07-12 12:30:10", "2023-07-12 11:30:10"; "different hour")]
    #[test_case("Jul 12", "2023-07-12 12:30:10", "2023-07-13 12:30:10"; "different day")]
    #[test_case("Jul 12", "2023-07-12 12:30:10", "2023-07-13 12:30:10"; "different month")]
    #[test_case("Jul 12, 2023", "2023-07-12 12:30:10", "2022-07-13 12:30:10"; "different year")]
    fn humanize_datetime_is_correct(expected: &str, datetime: &str, relative_to: &str) {
        let result = humanize_datetime(to_local_datetime(datetime), to_local_datetime(relative_to));

        assert_eq!(expected, result);
    }

    fn to_local_datetime(datetime: &str) -> DateTime<Local> {
        let datetime = NaiveDateTime::parse_from_str(datetime, DATETIME_FORMAT).unwrap();
        Local.from_local_datetime(&datetime).unwrap()
    }
}
