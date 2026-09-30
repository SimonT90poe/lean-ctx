use crate::core::measurement::{MeasurementFramework, MeasurementMode, report};

pub(crate) fn cmd_measure(args: &[String]) {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
    {
        print_usage();
        return;
    }
    let framework = MeasurementFramework::from_data_dir();
    match args.first().map(String::as_str) {
        Some("baseline-start") => set_mode(&framework, MeasurementMode::Baseline),
        Some("baseline-stop" | "treatment-stop") => {
            set_mode(&framework, MeasurementMode::Off);
        }
        Some("treatment-start") => set_mode(&framework, MeasurementMode::Treatment),
        Some("compare") if args.len() == 1 => print_report(&framework, "markdown"),
        Some("report") => match parse_format(&args[1..]) {
            Some(format) => print_report(&framework, format),
            None => usage_error(),
        },
        _ => usage_error(),
    }
}

fn set_mode(framework: &MeasurementFramework, mode: MeasurementMode) {
    if let Err(error) = framework.set_mode(mode) {
        eprintln!("measure: {error}");
        std::process::exit(1);
    }
    println!("Measurement mode: {}", mode.as_str());
}

fn print_report(framework: &MeasurementFramework, format: &str) {
    match framework.comparison() {
        Ok(comparison) => match format {
            "markdown" => print!("{}", report::markdown(&comparison)),
            "json" => println!("{}", report::json(&comparison)),
            _ => usage_error(),
        },
        Err(error) => {
            eprintln!("measure: {error}");
            std::process::exit(1);
        }
    }
}

fn parse_format(args: &[String]) -> Option<&str> {
    match args {
        [] => Some("markdown"),
        [flag, format] if flag == "--format" && matches!(format.as_str(), "markdown" | "json") => {
            Some(format)
        }
        _ => None,
    }
}

fn usage_error() {
    print_usage();
    std::process::exit(2);
}
fn print_usage() {
    println!(
        "Usage: lean-ctx measure <baseline-start|baseline-stop|treatment-start|treatment-stop|compare|report> [--format markdown|json]"
    );
}
