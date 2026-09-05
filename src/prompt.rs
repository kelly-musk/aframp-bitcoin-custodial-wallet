use std::io::{self, Write};

use anyhow::{Context, Result};

/// Prompts on stdout, reads a line from stdin, returns `default` (if any) on empty input.
pub fn line(question: &str, default: Option<&str>) -> Result<String> {
    match default {
        Some(d) => print!("{question} [{d}]: "),
        None => print!("{question}: "),
    }
    io::stdout().flush().ok();

    let mut input = String::new();
    io::stdin().read_line(&mut input).context("reading input")?;
    let input = input.trim();
    Ok(if input.is_empty() { default.unwrap_or("").to_string() } else { input.to_string() })
}

/// Prompts until the answer case-insensitively matches one of `options`; returns the matched
/// canonical spelling.
pub fn choice<'a>(question: &str, options: &[&'a str], default: &'a str) -> Result<&'a str> {
    let joined = options.join("/");
    loop {
        let answer = line(&format!("{question} ({joined})"), Some(default))?;
        if let Some(&matched) = options.iter().find(|o| o.eq_ignore_ascii_case(&answer)) {
            return Ok(matched);
        }
        println!("please answer one of: {joined}");
    }
}

/// Prints a numbered list and prompts until the answer is a valid index or an exact item.
pub fn pick(question: &str, items: &[String]) -> Result<String> {
    for (i, item) in items.iter().enumerate() {
        println!("  {}) {item}", i + 1);
    }
    loop {
        let answer = line(question, None)?;
        if let Ok(idx) = answer.parse::<usize>()
            && idx >= 1
            && idx <= items.len()
        {
            return Ok(items[idx - 1].clone());
        }
        if let Some(m) = items.iter().find(|i| **i == answer) {
            return Ok(m.clone());
        }
        println!("please enter a number from 1-{} or one of the names above", items.len());
    }
}
