//! Usage accounting: disjoint buckets, cache fields only when non-zero,
//! cumulative merge across chunks, and the fail-closed incomplete flag.

use craft_core::types::Usage;

fn tokens(input: u64, output: u64) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        incomplete: false,
    }
}

fn with_cache(record: Usage, cache_read: u64, cache_write: u64) -> Usage {
    Usage {
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        ..record
    }
}

fn with_reasoning(record: Usage, reasoning: u64) -> Usage {
    Usage {
        reasoning_tokens: reasoning,
        ..record
    }
}

fn flagged_incomplete(record: Usage) -> Usage {
    Usage {
        incomplete: true,
        ..record
    }
}

#[test]
fn buckets_are_disjoint_fields() -> Result<(), serde_json::Error> {
    let record = with_reasoning(with_cache(tokens(10, 40), 20, 30), 50);
    assert_eq!(record.input_tokens, 10);
    assert_eq!(record.cache_read_tokens, 20);
    assert_eq!(record.cache_write_tokens, 30);
    assert_eq!(record.output_tokens, 40);
    assert_eq!(record.reasoning_tokens, 50);
    let json = serde_json::to_string(&record)?;
    assert!(json.contains("\"cache_read_tokens\":20"));
    assert!(json.contains("\"cache_write_tokens\":30"));
    let back: Usage = serde_json::from_str(&json)?;
    assert_eq!(back, record);
    Ok(())
}

#[test]
fn zero_cache_fields_are_omitted_from_json() -> Result<(), serde_json::Error> {
    let record = tokens(10, 5);
    let json = serde_json::to_string(&record)?;
    assert!(
        !json.contains("cache_read_tokens"),
        "zero cache-read must not serialize: {json}"
    );
    assert!(
        !json.contains("cache_write_tokens"),
        "zero cache-write must not serialize: {json}"
    );
    assert!(
        json.contains("\"reasoning_tokens\":0"),
        "only cache fields are optional: {json}"
    );
    Ok(())
}

#[test]
fn nonzero_cache_fields_serialize() -> Result<(), serde_json::Error> {
    let record = with_cache(tokens(10, 5), 7, 3);
    let json = serde_json::to_string(&record)?;
    assert!(json.contains("\"cache_read_tokens\":7"));
    assert!(json.contains("\"cache_write_tokens\":3"));
    Ok(())
}

#[test]
fn absent_fields_default_to_zero_and_complete() -> Result<(), serde_json::Error> {
    let record: Usage =
        serde_json::from_str("{\"input_tokens\":4,\"output_tokens\":2,\"reasoning_tokens\":1}")?;
    assert_eq!(record, with_reasoning(tokens(4, 2), 1));
    Ok(())
}

#[test]
fn omitted_usage_is_fail_closed() {
    let record = Usage::omitted();
    assert_eq!(record.input_tokens, 0);
    assert_eq!(record.output_tokens, 0);
    assert!(record.incomplete, "omitted usage must flag incompleteness");
}

#[test]
fn incomplete_flag_round_trips() -> Result<(), serde_json::Error> {
    let record = flagged_incomplete(tokens(1, 2));
    let json = serde_json::to_string(&record)?;
    assert!(
        json.contains("\"incomplete\":true"),
        "the flag must serialize when set: {json}"
    );
    let back: Usage = serde_json::from_str(&json)?;
    assert!(back.incomplete);
    Ok(())
}

#[test]
fn merge_takes_the_larger_bucket_not_the_sum() {
    // Providers send cumulative counters; merging must not double-count.
    let early = tokens(10, 2);
    let later = tokens(10, 5);
    assert_eq!(early.merge(later), tokens(10, 5));
}

#[test]
fn merge_fills_buckets_only_one_chunk_reports() {
    let first = with_cache(tokens(10, 0), 4, 0);
    let second = with_reasoning(with_cache(tokens(0, 5), 0, 3), 2);
    assert_eq!(
        first.merge(second),
        with_reasoning(with_cache(tokens(10, 5), 4, 3), 2)
    );
}

#[test]
fn merge_keeps_cache_and_reasoning_buckets_disjoint() {
    let first = with_reasoning(with_cache(tokens(10, 5), 4, 3), 2);
    let second = with_reasoning(with_cache(tokens(9, 4), 3, 2), 1);
    let merged = first.merge(second);
    assert_eq!(merged.input_tokens, 10);
    assert_eq!(merged.cache_read_tokens, 4);
    assert_eq!(merged.cache_write_tokens, 3);
    assert_eq!(merged.output_tokens, 5);
    assert_eq!(merged.reasoning_tokens, 2);
}

#[test]
fn merge_fails_closed_on_any_incomplete_report() {
    let complete = tokens(10, 5);
    let incomplete = Usage::omitted();
    assert!(complete.merge(incomplete).incomplete);
    assert!(incomplete.merge(complete).incomplete);
    assert!(!complete.merge(tokens(1, 1)).incomplete);
}

#[test]
fn merge_is_associative() {
    let a = tokens(3, 0);
    let b = tokens(3, 7);
    let c = tokens(2, 7);
    assert_eq!(a.merge(b).merge(c), a.merge(b.merge(c)));
}
