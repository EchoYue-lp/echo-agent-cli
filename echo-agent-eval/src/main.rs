//! echo-agent-eval — CLI entry point.

use clap::{Parser, Subcommand};
use echo_agent_app_core::config::load_config;
use echo_agent_app_core::infra::AgentCreateParams;
use echo_agent_app_core::runtime::AgentRuntime;
use echo_agent_eval::case_loader;
use echo_agent_eval::eval_runner;
use echo_agent_eval::trigger_test;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "echo-agent-eval")]
#[command(about = "Independent evaluation harness for echo-agent")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run eval cases for a domain
    Run {
        /// Domain to evaluate (coding, data-analysis, research, medical, general, skill-trigger)
        /// Use "all" or omit to run all domains.
        domain: Option<String>,

        /// Path to eval cases directory
        #[arg(long)]
        cases_dir: Option<PathBuf>,

        /// Maximum number of cases to run
        #[arg(long)]
        cases: Option<usize>,

        /// Timeout per case in seconds
        #[arg(long, default_value = "300")]
        timeout: u64,

        /// HTML report output directory
        #[arg(long)]
        output: Option<PathBuf>,

        /// Override model name
        #[arg(long)]
        model: Option<String>,
    },

    /// List available eval cases
    List {
        /// Filter by domain
        domain: Option<String>,

        /// Path to eval cases directory
        #[arg(long)]
        cases_dir: Option<PathBuf>,
    },

    /// Run skill trigger accuracy test
    TriggerTest {
        /// Path to eval cases directory
        #[arg(long)]
        cases_dir: Option<PathBuf>,

        /// F1 score threshold (default 0.85)
        #[arg(long, default_value = "0.85")]
        threshold: f64,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            domain,
            cases_dir,
            cases,
            timeout,
            output,
            model,
        } => {
            cmd_run(domain, cases_dir, cases, timeout, output, model).await?;
        }
        Commands::List { domain, cases_dir } => {
            cmd_list(domain, cases_dir)?;
        }
        Commands::TriggerTest {
            cases_dir,
            threshold,
        } => {
            cmd_trigger_test(cases_dir, threshold)?;
        }
    }

    Ok(())
}

async fn cmd_run(
    domain: Option<String>,
    cases_dir: Option<PathBuf>,
    max_cases: Option<usize>,
    timeout_secs: u64,
    output_dir: Option<PathBuf>,
    model_override: Option<String>,
) -> anyhow::Result<()> {
    let dir = cases_dir.unwrap_or_else(case_loader::default_cases_dir);

    let domains: Vec<&str> = match domain.as_deref() {
        Some("all") | None => vec![],
        Some(d) => vec![d],
    };

    let all_cases =
        case_loader::load_cases_by_domain(&dir, &domains).map_err(|e| anyhow::anyhow!(e))?;

    if all_cases.is_empty() {
        println!("No eval cases found for domain: {:?}", domain);
        return Ok(());
    }

    let cases: Vec<_> = if let Some(max) = max_cases {
        all_cases.into_iter().take(max).collect()
    } else {
        all_cases
    };

    println!(
        "\n🧪 Running {} eval cases (timeout: {}s per case)...\n",
        cases.len(),
        timeout_secs
    );

    // Create agent via full runtime bootstrap (skills, MCP, hooks, IntentRouter, etc.)
    let app_config = load_config(None);
    let params = AgentCreateParams {
        model: model_override,
        system_prompt: None,
        project: None,
        session_id: Some(format!("eval-{}", uuid::Uuid::new_v4())),
        conversation_id: None,
        react_checkpoint_interval: None,
        state_store: None,
        memory_context_suffix: None,
    };
    let runtime = AgentRuntime::bootstrap(&app_config, params).await?;
    let agent = runtime.agent_handle.as_shared_agent().await;

    // Run eval
    let report = eval_runner::run_cases(&*agent, &cases, timeout_secs).await;
    println!("{}", eval_runner::format_report(&report, domain.as_deref()));

    // Save HTML report
    if let Some(dir) = output_dir {
        match eval_runner::save_report(&report, &dir, domain.as_deref()) {
            Ok(path) => println!("📄 Report saved: {}", path.display()),
            Err(e) => eprintln!("⚠️  Failed to save report: {e}"),
        }
    }

    Ok(())
}

fn cmd_list(domain: Option<String>, cases_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let dir = cases_dir.unwrap_or_else(case_loader::default_cases_dir);

    let domains: Vec<&str> = domain
        .as_ref()
        .filter(|s| s.as_str() != "all")
        .map(|s| vec![s.as_str()])
        .unwrap_or_default();

    let cases =
        case_loader::load_cases_by_domain(&dir, &domains).map_err(|e| anyhow::anyhow!(e))?;

    if cases.is_empty() {
        println!("No eval cases found.");
        return Ok(());
    }

    println!("\n📋 Available eval cases ({} total):\n", cases.len());
    let mut current_domain = String::new();
    for case in &cases {
        let d = case.domain.as_deref().unwrap_or("uncategorized");
        if d != current_domain {
            current_domain = d.to_string();
            println!("\n  [{current_domain}]");
        }
        println!(
            "    {:20} {}",
            case.id,
            if case.description.is_empty() {
                &case.name
            } else {
                &case.description
            }
        );
    }
    println!();

    Ok(())
}

fn cmd_trigger_test(cases_dir: Option<PathBuf>, threshold: f64) -> anyhow::Result<()> {
    let dir = cases_dir.unwrap_or_else(case_loader::default_cases_dir);

    // Build a keyword→skill map by loading SKILL.md triggers from the skills directory
    let skills_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("skills");

    let trigger_map = load_skill_triggers(&skills_dir);
    let match_fn = move |query: &str| -> Option<String> {
        let lower = query.to_lowercase();
        for (trigger, skill_name) in &trigger_map {
            if lower.contains(trigger) {
                return Some(skill_name.clone());
            }
        }
        None
    };

    trigger_test::run_trigger_test(match_fn, &dir, threshold).map_err(|e| anyhow::anyhow!(e))?;
    Ok(())
}

/// Load trigger keywords from SKILL.md files in a directory.
///
/// Returns a map of trigger_keyword → skill_name.
fn load_skill_triggers(skills_dir: &std::path::Path) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    if !skills_dir.is_dir() {
        eprintln!(
            "Warning: skills directory not found: {}",
            skills_dir.display()
        );
        return map;
    }

    for entry in std::fs::read_dir(skills_dir).unwrap_or_else(|_| {
        eprintln!("Warning: cannot read skills directory");
        std::fs::read_dir(".").unwrap()
    }) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let skill_md = entry.path().join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }

        let content = match std::fs::read_to_string(&skill_md) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Parse YAML frontmatter
        let frontmatter = content
            .strip_prefix("---")
            .and_then(|rest| rest.split_once("---"))
            .map(|(fm, _)| fm);

        if let Some(fm) = frontmatter
            && let Ok(yaml) = serde_yaml::from_str::<serde_yaml::Value>(fm)
        {
            let skill_name = yaml
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if let Some(triggers) = yaml.get("triggers").and_then(|v| v.as_sequence()) {
                for t in triggers {
                    if let Some(trigger) = t.as_str() {
                        map.insert(trigger.to_lowercase(), skill_name.clone());
                    }
                }
            }
        }
    }

    eprintln!(
        "Loaded {} triggers from {} skills",
        map.len(),
        map.values().collect::<std::collections::HashSet<_>>().len()
    );
    map
}
