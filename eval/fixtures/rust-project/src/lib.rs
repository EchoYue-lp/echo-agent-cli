//! Eval fixture: small Rust library with intentional bugs for coding eval.

/// Parse a simple key=value config string into a HashMap.
/// BUG: off-by-one error in the loop — fix it as part of eval.
pub fn parse_config(input: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let lines: Vec<&str> = input.lines().collect();
    // BUG: should be `i < lines.len()`, not `i <= lines.len()`
    let mut i = 0;
    while i <= lines.len() {
        let line = lines[i].trim();
        if let Some((key, value)) = line.split_once('=') {
            map.insert(key.trim().to_string(), value.trim().to_string());
        }
        i += 1;
    }
    map
}

/// Add two numbers.
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add() {
        assert_eq!(add(1, 2), 3);
        assert_eq!(add(-1, 1), 0);
        assert_eq!(add(0, 0), 0);
    }

    #[test]
    fn test_parse_config_basic() {
        let config = "key1=value1\nkey2=value2";
        let result = parse_config(config);
        assert_eq!(result.get("key1"), Some(&"value1".to_string()));
        assert_eq!(result.get("key2"), Some(&"value2".to_string()));
        assert_eq!(result.len(), 2);
    }
}
