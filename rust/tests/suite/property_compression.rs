use lean_ctx::core::compressor::safeguard_ratio;
// Property/fuzz tests MUST use the deterministic entry point. The default
// `entropy_compress` runs the #544 semantic-redundancy filter, which self-
// activates the shared neural embedding engine on first use (#551). The
// `cfg!(test)` guard that suppresses that background load only covers *unit*
// tests — this is an *integration* test, so the lib is linked without
// `cfg(test)` and the engine would load and run real per-line GEMM inference
// for every generated case, turning the proptest run into an effective hang on
// any machine that has the embedding model present. The deterministic path is
// purpose-built for run-to-run / machine-to-machine reproducibility and never
// touches the engine, so it exercises the same compression invariants without
// the nondeterministic, model-dependent cost.
use lean_ctx::core::entropy::entropy_compress_deterministic;
use lean_ctx::core::patterns::compress_output;
use lean_ctx::core::tokens::count_tokens;
use proptest::prelude::*;

proptest! {
    #[test]
    fn safeguard_never_inflates(
        original in "[a-zA-Z0-9 \n]{1,500}",
        compressed in "[a-zA-Z0-9 \n]{0,500}"
    ) {
        let result = safeguard_ratio(&original, &compressed);
        let result_tokens = count_tokens(&result);
        let orig_tokens = count_tokens(&original);
        let comp_tokens = count_tokens(&compressed);
        let max_tokens = orig_tokens.max(comp_tokens);
        prop_assert!(
            result_tokens <= max_tokens,
            "safeguard_ratio returned {} tokens, max(orig={}, comp={}) = {}",
            result_tokens, orig_tokens, comp_tokens, max_tokens
        );
    }

    #[test]
    fn safeguard_returns_one_of_inputs(
        original in "[a-zA-Z0-9 \n]{1,200}",
        compressed in "[a-zA-Z0-9 \n]{0,200}"
    ) {
        let result = safeguard_ratio(&original, &compressed);
        prop_assert!(
            result == original || result == compressed,
            "safeguard_ratio must return either original or compressed"
        );
    }

    #[test]
    fn entropy_compress_output_is_subset_of_lines(
        content in "[a-zA-Z0-9(){};= \n]{10,800}"
    ) {
        let result = entropy_compress_deterministic(&content);
        for line in result.output.lines() {
            if line.trim().is_empty() {
                continue;
            }
            prop_assert!(
                content.contains(line.trim()),
                "entropy output line not found in original: {:?}",
                line
            );
        }
    }

    #[test]
    fn entropy_compress_tokens_not_greater(
        content in "[a-zA-Z0-9(){};= \n]{10,600}"
    ) {
        let result = entropy_compress_deterministic(&content);
        prop_assert!(
            result.compressed_tokens <= result.original_tokens,
            "compressed {} > original {}",
            result.compressed_tokens, result.original_tokens
        );
    }

    #[test]
    fn compress_output_never_inflates_tokens(
        command in "(git status|cargo test|npm install|ls -la)",
        output in "[a-zA-Z0-9:/ \n._-]{20,1000}"
    ) {
        if let Some(compressed) = compress_output(&command, &output) {
            let orig_tokens = count_tokens(&output);
            let comp_tokens = count_tokens(&compressed);
            prop_assert!(
                comp_tokens <= orig_tokens,
                "compress_output inflated: {} > {} for command '{}'",
                comp_tokens, orig_tokens, command
            );
        }
    }
}

#[cfg(test)]
mod fuzz_no_panic {
    use super::*;

    proptest! {
        #[test]
        fn compress_output_no_panic_on_arbitrary_utf8(
            command in "\\PC{1,100}",
            output in "\\PC{0,2000}"
        ) {
            let _ = compress_output(&command, &output);
        }

        #[test]
        fn safeguard_no_panic_on_empty(
            text in "\\PC{0,100}"
        ) {
            let _ = safeguard_ratio("", &text);
            let _ = safeguard_ratio(&text, "");
        }

        #[test]
        fn entropy_compress_no_panic(
            content in "\\PC{0,1000}"
        ) {
            let _ = entropy_compress_deterministic(&content);
        }
    }
}
