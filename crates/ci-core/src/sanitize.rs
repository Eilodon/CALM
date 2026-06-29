use regex::Regex;
use std::sync::LazyLock;

struct CredentialPattern {
    regex: Regex,
    label: &'static str,
}

static CREDENTIAL_PATTERNS: LazyLock<Vec<CredentialPattern>> = LazyLock::new(|| {
    vec![
        CredentialPattern {
            regex: Regex::new(r"-----BEGIN (?:RSA |EC |DSA |OPENSSH )?PRIVATE KEY-----[\s\S]*?-----END (?:RSA |EC |DSA |OPENSSH )?PRIVATE KEY-----").unwrap(),
            label: "PEM_PRIVATE_KEY",
        },
        CredentialPattern {
            regex: Regex::new(r"(?i)(?:sk|rk)-[a-zA-Z0-9]{20,}").unwrap(),
            label: "SECRET_KEY",
        },
        CredentialPattern {
            regex: Regex::new(r"ghp_[a-zA-Z0-9]{36,}").unwrap(),
            label: "GITHUB_PAT",
        },
        CredentialPattern {
            regex: Regex::new(r"gho_[a-zA-Z0-9]{36,}").unwrap(),
            label: "GITHUB_OAUTH",
        },
        CredentialPattern {
            regex: Regex::new(r"ghs_[a-zA-Z0-9]{36,}").unwrap(),
            label: "GITHUB_APP",
        },
        CredentialPattern {
            regex: Regex::new(r"ghr_[a-zA-Z0-9]{36,}").unwrap(),
            label: "GITHUB_REFRESH",
        },
        CredentialPattern {
            regex: Regex::new(r"AKIA[A-Z0-9]{16}").unwrap(),
            label: "AWS_ACCESS_KEY",
        },
        CredentialPattern {
            regex: Regex::new(r#"(?i)(?:password|passwd|secret|api_key|apikey|access_token|auth_token)\s*[=:]\s*["'][^\s"']{8,}["']"#).unwrap(),
            label: "CREDENTIAL_ASSIGNMENT",
        },
        CredentialPattern {
            regex: Regex::new(r"eyJ[a-zA-Z0-9_-]{20,}\.eyJ[a-zA-Z0-9_-]{20,}\.[a-zA-Z0-9_-]{20,}").unwrap(),
            label: "JWT_TOKEN",
        },
        CredentialPattern {
            regex: Regex::new(r"xox[bpoas]-[a-zA-Z0-9-]{10,}").unwrap(),
            label: "SLACK_TOKEN",
        },
    ]
});

pub fn sanitize_source_output(code: &str) -> String {
    let mut result = code.to_string();
    for pattern in CREDENTIAL_PATTERNS.iter() {
        result = pattern
            .regex
            .replace_all(&result, format!("[REDACTED:{}]", pattern.label))
            .into_owned();
    }
    result
}

pub fn contains_credentials(code: &str) -> bool {
    CREDENTIAL_PATTERNS.iter().any(|p| p.regex.is_match(code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_credentials() {
        let code = "fn main() {\n    println!(\"hello\");\n}";
        assert_eq!(sanitize_source_output(code), code);
        assert!(!contains_credentials(code));
    }

    #[test]
    fn test_redact_github_pat() {
        let code = r#"token = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij""#;
        let sanitized = sanitize_source_output(code);
        assert!(sanitized.contains("[REDACTED:GITHUB_PAT]"));
        assert!(!sanitized.contains("ghp_"));
    }

    #[test]
    fn test_redact_aws_key() {
        let code = "aws_key = \"AKIAIOSFODNN7EXAMPLE\"";
        let sanitized = sanitize_source_output(code);
        assert!(sanitized.contains("[REDACTED:AWS_ACCESS_KEY]"));
        assert!(!sanitized.contains("AKIA"));
    }

    #[test]
    fn test_redact_password_assignment() {
        let code = r#"password = "supersecretpass123""#;
        let sanitized = sanitize_source_output(code);
        assert!(sanitized.contains("[REDACTED:CREDENTIAL_ASSIGNMENT]"));
    }

    #[test]
    fn test_redact_pem_key() {
        let code =
            "-----BEGIN RSA PRIVATE KEY-----\nMIIE...base64...\n-----END RSA PRIVATE KEY-----";
        let sanitized = sanitize_source_output(code);
        assert!(sanitized.contains("[REDACTED:PEM_PRIVATE_KEY]"));
    }

    #[test]
    fn test_redact_jwt() {
        let code = "token = eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let sanitized = sanitize_source_output(code);
        assert!(sanitized.contains("[REDACTED:JWT_TOKEN]"));
    }

    #[test]
    fn test_redact_slack_token() {
        let code = r#"SLACK = "xoxb-123456789-abcdefghij""#;
        let sanitized = sanitize_source_output(code);
        assert!(sanitized.contains("[REDACTED:SLACK_TOKEN]"));
    }

    #[test]
    fn test_redact_secret_key() {
        let code = r#"key = "sk-proj1234567890abcdefghij""#;
        let sanitized = sanitize_source_output(code);
        assert!(sanitized.contains("[REDACTED:SECRET_KEY]"));
    }

    #[test]
    fn test_contains_credentials_true() {
        assert!(contains_credentials(
            "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij"
        ));
    }

    #[test]
    fn test_contains_credentials_false() {
        assert!(!contains_credentials("fn main() {}"));
    }

    #[test]
    fn test_multiple_credentials() {
        let code = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij\nAKIAIOSFODNN7EXAMPLE";
        let sanitized = sanitize_source_output(code);
        assert!(sanitized.contains("[REDACTED:GITHUB_PAT]"));
        assert!(sanitized.contains("[REDACTED:AWS_ACCESS_KEY]"));
    }
}
