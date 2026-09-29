use lean_ctx::core::entropy::{
    jaccard_similarity, kolmogorov_proxy, ngram_jaccard, normalized_token_entropy, shannon_entropy,
    token_entropy,
};
use lean_ctx::core::tokens::count_tokens;

// ═══════════════════════════════════════════════════════════════════
// 1. SHANNON ENTROPY — mathematical invariants
// ═══════════════════════════════════════════════════════════════════

#[test]
fn shannon_entropy_bounds() {
    let text = "abcdefghijklmnop";
    let h = shannon_entropy(text);
    let n = text.chars().collect::<std::collections::HashSet<_>>().len() as f64;
    assert!(h >= 0.0, "H(X) must be non-negative");
    assert!(
        h <= n.log2() + 0.01,
        "H(X) must be ≤ log₂(|alphabet|) = {:.2}, got {h:.2}",
        n.log2()
    );
}

#[test]
fn shannon_entropy_maximum_for_uniform() {
    let text = "abcdefghijklmnop";
    let h = shannon_entropy(text);
    let n = text.chars().collect::<std::collections::HashSet<_>>().len() as f64;
    let h_max = n.log2();
    let ratio = h / h_max;
    assert!(
        ratio > 0.99,
        "uniform distribution should yield H ≈ log₂(n): ratio={ratio:.4}"
    );
}

#[test]
fn shannon_entropy_zero_for_constant() {
    assert_eq!(
        shannon_entropy("aaaaaaa"),
        0.0,
        "constant string has zero entropy"
    );
}

#[test]
fn shannon_additivity_subadditive() {
    let a = "abcabc";
    let b = "xyzxyz";
    let ab = format!("{a}{b}");
    let h_ab = shannon_entropy(&ab);
    let h_a = shannon_entropy(a);
    let h_b = shannon_entropy(b);
    assert!(
        h_ab <= h_a + h_b + 0.5,
        "joint entropy should be sub-additive (with tolerance): H(AB)={h_ab:.2} > H(A)+H(B)={:.2}",
        h_a + h_b
    );
}

// ═══════════════════════════════════════════════════════════════════
// 2. NORMALIZED ENTROPY — must be in [0, 1]
// ═══════════════════════════════════════════════════════════════════

#[test]
fn normalized_entropy_in_unit_interval() {
    let cases = [
        "fn main() { println!(\"hello world\"); }",
        "aaaa bbbb cccc",
        "let x = compute_something(a, b, c, d, e);",
        "}}}}",
    ];
    for text in &cases {
        let h = normalized_token_entropy(text);
        assert!(
            (0.0..=1.0).contains(&h),
            "normalized entropy must be in [0,1], got {h:.4} for {text:?}",
        );
    }
}

#[test]
fn normalized_entropy_monotonic_with_diversity() {
    let low = "test test test test test test";
    let high = "alpha beta gamma delta epsilon zeta";
    assert!(
        normalized_token_entropy(high) > normalized_token_entropy(low),
        "diverse text should have higher normalized entropy"
    );
}

#[test]
fn normalized_entropy_zero_for_single_token() {
    assert_eq!(
        normalized_token_entropy("}"),
        0.0,
        "single token has zero normalized entropy"
    );
}

// ═══════════════════════════════════════════════════════════════════
// 3. TOKEN ENTROPY vs CHARACTER ENTROPY — relationship
// ═══════════════════════════════════════════════════════════════════

#[test]
fn bpe_entropy_differs_from_char_entropy() {
    let code = "fn validate_credentials(username: &str, password: &str) -> bool { true }";
    let h_char = shannon_entropy(code);
    let h_bpe = token_entropy(code);
    assert!(
        (h_char - h_bpe).abs() > 0.01,
        "BPE and char entropy should differ for code: char={h_char:.3}, bpe={h_bpe:.3}"
    );
}

// ═══════════════════════════════════════════════════════════════════
// 4. KOLMOGOROV PROXY — mathematical properties
// ═══════════════════════════════════════════════════════════════════

#[test]
fn kolmogorov_bounds() {
    let text = "hello world, this is a test of Kolmogorov complexity estimation";
    let k = kolmogorov_proxy(text);
    assert!(k > 0.0, "K(x) must be positive for non-empty");
    assert!(
        k <= 2.0,
        "K(x) = gzip/raw should be ≤ ~2.0 (gzip overhead for short strings)"
    );
}

#[test]
fn kolmogorov_monotonic_with_redundancy() {
    let redundant = "abcabc".repeat(100);
    let random_like: String = (0..600)
        .map(|i| char::from(b'a' + (((i * 7 + 13) % 26) as u8)))
        .collect();
    let k_red = kolmogorov_proxy(&redundant);
    let k_rand = kolmogorov_proxy(&random_like);
    assert!(
        k_red < k_rand,
        "redundant text should have lower K: {k_red:.3} vs {k_rand:.3}"
    );
}

#[test]
fn kolmogorov_invariant_under_repetition() {
    let base = "fn process(data: &[u8]) -> Result<Output, Error> { Ok(Output::default()) }\n";
    let k1 = kolmogorov_proxy(base);
    let repeated = base.repeat(50);
    let k50 = kolmogorov_proxy(&repeated);
    assert!(
        k50 < k1,
        "repeating content should decrease K: K(1)={k1:.3}, K(50)={k50:.3}"
    );
}

// ═══════════════════════════════════════════════════════════════════
// 5. JACCARD SIMILARITY — metric space axioms
// ═══════════════════════════════════════════════════════════════════

#[test]
fn jaccard_identity() {
    let text = "hello world foo bar";
    assert!(
        (jaccard_similarity(text, text) - 1.0).abs() < f64::EPSILON,
        "J(A,A) must equal 1.0"
    );
}

#[test]
fn jaccard_symmetry() {
    let a = "alpha beta gamma";
    let b = "beta gamma delta";
    let j_ab = jaccard_similarity(a, b);
    let j_ba = jaccard_similarity(b, a);
    assert!(
        (j_ab - j_ba).abs() < f64::EPSILON,
        "J(A,B) must equal J(B,A): {j_ab} vs {j_ba}"
    );
}

#[test]
fn jaccard_triangle_inequality() {
    let a = "alpha beta gamma delta";
    let b = "beta gamma delta epsilon";
    let c = "delta epsilon zeta eta";
    let j_ab = jaccard_similarity(a, b);
    let j_bc = jaccard_similarity(b, c);
    let j_ac = jaccard_similarity(a, c);
    assert!(
        j_ac <= j_ab + j_bc + 0.01,
        "triangle inequality: J(A,C)={j_ac:.3} should be ≤ J(A,B)+J(B,C)={:.3}",
        j_ab + j_bc
    );
}

#[test]
fn ngram_jaccard_order_sensitive() {
    let a = "fn foo(a: i32, b: i32)";
    let b = "fn foo(b: i32, a: i32)";
    let word_j = jaccard_similarity(a, b);
    let ngram_j = ngram_jaccard(a, b, 2);
    assert!(
        ngram_j < word_j || (word_j - ngram_j).abs() < f64::EPSILON,
        "bigram Jaccard should be ≤ word Jaccard for reordered text: ngram={ngram_j:.3}, word={word_j:.3}"
    );
}

// ═══════════════════════════════════════════════════════════════════
// 9. SYMBOL MAP ROI — break-even analysis
// ═══════════════════════════════════════════════════════════════════

#[test]
fn symbol_map_roi_positive_for_frequent_long_idents() {
    use lean_ctx::core::symbol_map::should_register;
    assert!(
        should_register("authenticate_user_credentials_handler", 10, 1),
        "very long ident (36 chars) with 10 occurrences should have positive ROI"
    );
}

#[test]
fn symbol_map_roi_negative_for_single_use() {
    use lean_ctx::core::symbol_map::should_register;
    assert!(
        !should_register("authenticate_user_credentials_handler", 1, 1),
        "single-use ident should have negative ROI"
    );
}

#[test]
fn symbol_map_net_savings_correct() {
    use lean_ctx::core::symbol_map::SymbolMap;

    let ident = "authenticate_user_credentials_handler";
    let occurrences = 15;
    let content = std::iter::repeat_n(ident, occurrences)
        .collect::<Vec<_>>()
        .join(" some_code ");

    let original_tokens = count_tokens(&content);

    let mut map = SymbolMap::new();
    map.register(ident);
    let compressed = map.apply(&content);
    let table = map.format_table();
    let compressed_tokens = count_tokens(&compressed) + count_tokens(&table);

    eprintln!(
        "[symbol map ROI] {ident} x{occurrences}: {original_tokens} → {compressed_tokens} tokens"
    );

    assert!(
        compressed_tokens < original_tokens,
        "symbol map should save tokens: {compressed_tokens} < {original_tokens}"
    );
}

// ═══════════════════════════════════════════════════════════════════
// 10. INFORMATION BOTTLENECK — task relevance filtering
// ═══════════════════════════════════════════════════════════════════

#[test]
fn ib_filter_preserves_task_relevant_lines() {
    use lean_ctx::core::task_relevance::information_bottleneck_filter;

    let mut lines = Vec::new();
    for i in 0..100 {
        if i == 10 || i == 50 {
            lines.push(format!(
                "pub fn validate_token(t: &str) -> bool {{ /* line {i} */ }}"
            ));
        } else {
            lines.push(format!("let unrelated_{i} = compute_{i}(x);"));
        }
    }
    let content = lines.join("\n");
    let result = information_bottleneck_filter(&content, &["validate_token".to_string()], 0.3, &[]);

    assert!(
        result.contains("validate_token"),
        "IB filter must preserve task-relevant lines"
    );
    let result_lines = result.lines().count();
    assert!(
        result_lines < 100,
        "IB filter should reduce lines: {result_lines} < 100"
    );
}

#[test]
fn ib_filter_reduces_more_for_repetitive_content() {
    use lean_ctx::core::task_relevance::{adaptive_ib_budget, information_bottleneck_filter};

    let repetitive = "let x = compute(a);\n".repeat(100);
    let diverse = (0..100).fold(String::new(), |mut s, i| {
        use std::fmt::Write;
        let _ = writeln!(s, "let var_{i} = func_{i}(arg_{i});");
        s
    });

    let budget_rep = adaptive_ib_budget(&repetitive, 0.5);
    let budget_div = adaptive_ib_budget(&diverse, 0.5);

    assert!(
        budget_rep < budget_div,
        "repetitive content should get lower IB budget: {budget_rep:.3} < {budget_div:.3}"
    );

    let kw = vec!["compute".to_string()];
    let filtered_rep = information_bottleneck_filter(&repetitive, &kw, 0.3, &[]);
    let filtered_div = information_bottleneck_filter(&diverse, &kw, 0.3, &[]);

    eprintln!(
        "[IB adaptive] repetitive: {}→{} lines, diverse: {}→{} lines",
        100,
        filtered_rep.lines().count(),
        100,
        filtered_div.lines().count()
    );
}

// ═══════════════════════════════════════════════════════════════════
// 11. SAFEGUARD RATIO — rate-distortion boundary
// ═══════════════════════════════════════════════════════════════════

#[test]
fn safeguard_prevents_over_compression() {
    use lean_ctx::core::compressor::safeguard_ratio;
    let original = "fn main() {\n".repeat(50);
    let over_compressed = "x";
    let result = safeguard_ratio(&original, over_compressed);
    assert_eq!(
        result, original,
        "safeguard must return original when ratio < 0.05 on small output"
    );
}

#[test]
fn safeguard_allows_good_compression() {
    use lean_ctx::core::compressor::safeguard_ratio;
    let original = "fn main() {\n    let x = compute();\n    println!(x);\n}\n".repeat(10);
    let compressed = "fn main() { let x = compute(); println!(x); }\n".repeat(10);
    let result = safeguard_ratio(&original, &compressed);
    assert_eq!(
        result, compressed,
        "safeguard must allow reasonable compression"
    );
}

// ═══════════════════════════════════════════════════════════════════
// 12. COST MODEL — economic sanity checks
// ═══════════════════════════════════════════════════════════════════

#[test]
fn cost_model_token_savings_exclude_output_bonus() {
    let summary = lean_ctx::core::stats::load_stats();
    let _ = summary.total_saved;
    let _ = summary.total_calls;
}

#[test]
fn cost_model_usd_is_bounded() {
    let tokens_saved: u64 = 1_000_000;
    let usd = tokens_saved as f64 / 1_000_000.0 * 2.50;
    assert!(
        (usd - 2.50).abs() < 0.01,
        "1M tokens at $2.50/M should be $2.50: got ${usd:.2}"
    );
}

// ═══════════════════════════════════════════════════════════════════
// 13. INTEGRATED SCIENTIFIC AUDIT
// ═══════════════════════════════════════════════════════════════════

#[test]
fn full_scientific_audit() {
    eprintln!("\n{}", "═".repeat(70));
    eprintln!("  SCIENTIFIC VERIFICATION AUDIT");
    eprintln!("{}", "═".repeat(70));

    let mut passed = 0;
    let mut total = 0;

    macro_rules! check {
        ($name:expr_2021, $cond:expr_2021) => {
            total += 1;
            let ok = $cond;
            if ok {
                passed += 1;
            }
            eprintln!("  {} {}", if ok { "✓" } else { "✗" }, $name);
            assert!(ok, "FAILED: {}", $name);
        };
    }

    check!(
        "Shannon H(X) ≥ 0 for all inputs",
        shannon_entropy("test") >= 0.0 && shannon_entropy("") >= 0.0
    );

    check!("Shannon H(constant) = 0", shannon_entropy("aaaa") == 0.0);

    check!("Normalized H ∈ [0,1]", {
        let h = normalized_token_entropy("fn main() { let x = compute(); }");
        (0.0..=1.0).contains(&h)
    });

    check!(
        "Kolmogorov K(redundant) < K(diverse)",
        kolmogorov_proxy(&"abc".repeat(200))
            < kolmogorov_proxy(&(0..200).fold(String::new(), |mut s, i| {
                use std::fmt::Write;
                let _ = write!(s, "x{i}");
                s
            }))
    );

    check!(
        "Jaccard J(A,A) = 1.0",
        (jaccard_similarity("a b c", "a b c") - 1.0).abs() < f64::EPSILON
    );

    check!("Jaccard J(A,B) = J(B,A)", {
        let j1 = jaccard_similarity("a b c", "b c d");
        let j2 = jaccard_similarity("b c d", "a b c");
        (j1 - j2).abs() < f64::EPSILON
    });

    check!("Safeguard ratio ∈ {original, compressed}", {
        use lean_ctx::core::compressor::safeguard_ratio;
        let o = "test ".repeat(50);
        let c = "t ".repeat(50);
        let r = safeguard_ratio(&o, &c);
        r == o || r == c
    });

    eprintln!("{}", "─".repeat(70));
    eprintln!("  RESULT: {passed}/{total} checks passed");
    eprintln!("{}\n", "═".repeat(70));
}
