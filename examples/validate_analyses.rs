use recall::config::load_analysis;
use recall::types::SectionSpec;
use std::path::Path;

fn main() {
    let files = ["analyses/iron-software-sales.yaml"];
    let mut failed = 0;
    for f in files {
        match load_analysis(Path::new(f)) {
            Ok(a) => {
                println!("OK  {} — \"{}\" ({} sections)", f, a.name, a.sections.len());
                for sec in &a.sections {
                    let kind = match &sec.spec {
                        SectionSpec::Questions { questions } => {
                            format!("questions ({} items)", questions.len())
                        }
                        SectionSpec::ScoredRubric { criteria } => {
                            format!("scored_rubric ({} criteria)", criteria.len())
                        }
                        SectionSpec::SignalCategorization { categories } => {
                            format!("signal_categorization ({} categories)", categories.len())
                        }
                        SectionSpec::Classification { options, .. } => {
                            format!("classification ({} options)", options.len())
                        }
                        SectionSpec::Summary { fields } => {
                            format!("summary ({} fields)", fields.len())
                        }
                    };
                    let opt = if sec.optional { " [optional]" } else { "" };
                    println!("      - {} :: {}{}", sec.id, kind, opt);
                }
            }
            Err(e) => {
                println!("ERR {}: {:#}", f, e);
                failed += 1;
            }
        }
    }
    if failed > 0 {
        std::process::exit(1);
    }
}
