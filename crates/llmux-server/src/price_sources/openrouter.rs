//! OpenRouter 适配器：`/api/v1/models` 是 JSON，价单位是「美元 / token」，
//! 这里换成上游统一的「美元 / 百万 token」。

use serde_json::Value;

use super::FetchedPrice;

pub fn parse(json: &Value) -> Vec<FetchedPrice> {
    crate::model_prices::parse_openrouter_models(json)
        .into_iter()
        .map(|p| FetchedPrice {
            model_id: p.id,
            vendor: Some(p.vendor),
            input: p.input * 1_000_000.0,
            output: p.output * 1_000_000.0,
            cache_read: p.cache_read.map(|v| v * 1_000_000.0),
            cache_write: p.cache_write.map(|v| v * 1_000_000.0),
            long_context_threshold: None,
            long_input: None,
            long_output: None,
            long_cache_read: None,
            long_cache_write: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn converts_per_token_to_per_million() {
        let json = json!({
            "data": [{
                "id": "deepseek/deepseek-v4.1-flash",
                "pricing": {
                    "prompt": "0.0000003",
                    "completion": "0.0000012",
                    "input_cache_read": "0.00000003"
                }
            }]
        });
        let v = parse(&json);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].model_id, "deepseek/deepseek-v4.1-flash");
        assert!((v[0].input - 0.30).abs() < 1e-9);
        assert!((v[0].output - 1.20).abs() < 1e-9);
        assert!((v[0].cache_read.unwrap() - 0.03).abs() < 1e-9);
        assert_eq!(v[0].cache_write, None);
    }
}
