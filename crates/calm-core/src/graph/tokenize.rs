use regex::Regex;
use std::sync::LazyLock;

static RE_UNDERSCORES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[_\-]+").unwrap());
static RE_CAMEL_LOWER_UPPER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([a-z0-9])([A-Z])").unwrap());
static RE_CAMEL_UPPER_UPPER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([A-Z]+)([A-Z][a-z])").unwrap());

pub fn tokenize_identifier(name: &str) -> String {
    let s = RE_UNDERSCORES.replace_all(name, " ");
    let s = RE_CAMEL_LOWER_UPPER.replace_all(&s, "$1 $2");
    let s = RE_CAMEL_UPPER_UPPER.replace_all(&s, "$1 $2");
    s.to_lowercase().trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_camel_case() {
        assert_eq!(tokenize_identifier("getUserByEmail"), "get user by email");
    }

    #[test]
    fn test_screaming_snake() {
        assert_eq!(tokenize_identifier("HTTP_STATUS_CODE"), "http status code");
    }

    #[test]
    fn test_mixed_case() {
        assert_eq!(tokenize_identifier("parseXMLFile"), "parse xml file");
    }

    #[test]
    fn test_snake_case() {
        assert_eq!(tokenize_identifier("parse_lcov_file"), "parse lcov file");
    }

    #[test]
    fn test_single_word() {
        assert_eq!(tokenize_identifier("hello"), "hello");
    }

    #[test]
    fn test_empty() {
        assert_eq!(tokenize_identifier(""), "");
    }

    #[test]
    fn test_https_request() {
        assert_eq!(tokenize_identifier("HTTPSRequest"), "https request");
    }

    #[test]
    fn test_kebab_case() {
        assert_eq!(
            tokenize_identifier("my-component-name"),
            "my component name"
        );
    }
}
