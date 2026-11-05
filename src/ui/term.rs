use colored::Colorize;

pub fn banner() {
    println!(
        "{}",
        "🌸 Piya — SEO · Dev · Security · PM".bright_magenta().bold()
    );
    println!("{}", "─".repeat(40).bright_black());
}

pub fn phase(icon: &str, label: &str, detail: &str) {
    println!(
        "{} {} {}",
        icon,
        label.bright_cyan().bold(),
        detail.dimmed()
    );
}

pub fn success(msg: &str) {
    println!("{} {}", "✓".green().bold(), msg.green());
}

pub fn warn(msg: &str) {
    println!("{} {}", "↻".yellow().bold(), msg.yellow());
}

pub fn stop(msg: &str) {
    println!("{} {}", "↯".red().bold(), msg.red());
}

pub fn box_result(title: &str, lines: &[(&str, String)]) {
    println!();
    println!("{}", format!("┌─ {title} ─").bright_green());
    for (k, v) in lines {
        println!("{} {} {}", "│".bright_black(), k.bold(), v);
    }
    println!("{}", "└─".bright_green());
    println!();
}
