use std::time::SystemTime;

use chrono::{DateTime, Local};

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut size = bytes as f64;
    let mut unit = 0;

    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else if size >= 100.0 {
        format!("{size:.0} {}", UNITS[unit])
    } else if size >= 10.0 {
        format!("{size:.1} {}", UNITS[unit])
    } else {
        format!("{size:.2} {}", UNITS[unit])
    }
}

pub fn format_system_time(time: SystemTime) -> String {
    let local: DateTime<Local> = time.into();
    local.format("%Y-%m-%d %H:%M").to_string()
}

pub fn truncate_middle(value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let char_count = value.chars().count();
    if char_count <= width {
        return value.to_string();
    }

    if width <= 1 {
        return ".".to_string();
    }

    if width <= 3 {
        return ".".repeat(width);
    }

    let keep = width - 3;
    let left = keep / 2;
    let right = keep - left;
    let start: String = value.chars().take(left).collect();
    let end: String = value
        .chars()
        .rev()
        .take(right)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{start}...{end}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.00 KiB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.00 MiB");
    }

    #[test]
    fn truncates_middle() {
        assert_eq!(truncate_middle("abcdef", 4), "...f");
        assert_eq!(truncate_middle("abc", 4), "abc");
    }
}
