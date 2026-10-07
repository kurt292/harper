//! Live test against a local Ollama server. Ignored by default; run with
//! `cargo test -p broadside-style --test ollama_live -- --ignored --nocapture`.

use broadside_style::model::{ModelConfig, available_models, check};
use broadside_style::sample_guides;

#[test]
#[ignore = "needs a running Ollama with the configured model pulled"]
fn style_check_against_local_model() {
    let config = ModelConfig::default();
    let models = available_models(&config).expect("Ollama reachable");
    assert!(
        models.iter().any(|m| m == &config.model),
        "model {} not pulled; have {models:?}",
        config.model
    );

    let text = "Hope you are doing well! The order was reviewed by our team and the invoice has \
                been sent. We can offer 15% off your next case of gummies.";
    let report = check(text, &sample_guides(), &config).expect("check succeeds");

    println!("{} findings in {} ms from {}", report.violations.len(), report.elapsed_ms, report.model);
    for v in &report.violations {
        println!(
            "  [{}] {:?} -> {:?} ({:.2}) {}",
            v.rule, v.original, v.suggestion, v.confidence, v.explanation
        );
    }
    for d in &report.dropped {
        println!("  dropped: {d}");
    }

    // Every surviving finding must be anchored to a real slice of the text.
    for v in &report.violations {
        let slice: String = text.chars().skip(v.span[0]).take(v.span[1] - v.span[0]).collect();
        assert_eq!(slice, v.original);
        assert!(v.rule.contains('·'), "rule id was not resolved to a label: {}", v.rule);
    }
}
