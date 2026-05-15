use anyhow::{Context, Result};

pub fn prompt_choice(label: &str, options: &[&str], default: &str) -> Result<String> {
    if options.len() == 1 {
        println!("{label}: {default}");
        return Ok(default.to_string());
    }
    eprint!("{label}");
    for (i, opt) in options.iter().enumerate() {
        let marker = if *opt == default { " (default)" } else { "" };
        eprint!("\n  {}) {opt}{marker}", i + 1);
    }
    eprint!("\nChoice [{}]: ", default);

    let input = read_line()?;
    if input.is_empty() {
        return Ok(default.to_string());
    }
    if let Ok(idx) = input.parse::<usize>()
        && idx >= 1
        && idx <= options.len()
    {
        return Ok(options[idx - 1].to_string());
    }
    if options.contains(&input.as_str()) {
        return Ok(input);
    }
    eprintln!("  Using default: {default}");
    Ok(default.to_string())
}

pub fn prompt_confirm(label: &str, default_yes: bool) -> Result<bool> {
    let hint = if default_yes { "Y/n" } else { "y/N" };
    eprint!("{label} [{hint}]: ");
    let input = read_line()?;
    if input.is_empty() {
        return Ok(default_yes);
    }
    match input.to_lowercase().as_str() {
        "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        _ => Ok(default_yes),
    }
}

pub fn prompt_secret(label: &str) -> Result<String> {
    eprint!("{label} (Enter to skip): ");
    read_line()
}

pub fn prompt_freetext(label: &str, default: &str) -> Result<String> {
    eprint!("{label} [{default}]: ");
    let input = read_line()?;
    if input.is_empty() {
        return Ok(default.to_string());
    }
    Ok(input)
}

pub fn read_line() -> Result<String> {
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("reading input")?;
    Ok(input.trim().to_string())
}

pub fn non_empty(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}
