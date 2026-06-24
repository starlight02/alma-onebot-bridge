use std::collections::HashMap;
use std::net::SocketAddr;

pub fn is_ws_authorized(
    expected_token: Option<&str>,
    auth_header: Option<&str>,
    query: &HashMap<String, String>,
) -> bool {
    let Some(expected) = expected_token.map(str::trim).filter(|t| !t.is_empty()) else {
        return true;
    };
    token_matches(expected, auth_header, query)
}

pub fn is_http_command_authorized(
    expected_token: Option<&str>,
    auth_header: Option<&str>,
    query: &HashMap<String, String>,
    remote: Option<SocketAddr>,
) -> bool {
    if remote.map(|addr| addr.ip().is_loopback()).unwrap_or(false) {
        return true;
    }

    expected_token
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(|expected| token_matches(expected, auth_header, query))
        .unwrap_or(false)
}

fn token_matches(
    expected: &str,
    auth_header: Option<&str>,
    query: &HashMap<String, String>,
) -> bool {
    auth_header
        .and_then(extract_authorization_token)
        .or_else(|| {
            query
                .get("access_token")
                .or_else(|| query.get("token"))
                .map(String::as_str)
        })
        .map(|token| token.trim() == expected)
        .unwrap_or(false)
}

fn extract_authorization_token(header: &str) -> Option<&str> {
    let trimmed = header.trim();
    // RFC 7235: the auth scheme (Bearer, Token, …) is case-insensitive.
    // Only `Bearer` and `Token` carry a credential we understand; headers
    // with no space (raw token) are accepted for compatibility with OneBot
    // clients that send the bare token without a scheme.
    let token = match trimmed.find(' ') {
        Some(space) => {
            let scheme = trimmed[..space].trim();
            let value = trimmed[space..].trim();
            if value.is_empty() {
                return None;
            }
            if scheme.eq_ignore_ascii_case("Bearer") || scheme.eq_ignore_ascii_case("Token") {
                value
            } else {
                return None;
            }
        }
        None => trimmed,
    };
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

#[cfg(test)]
mod tests {
    use super::{is_http_command_authorized, is_ws_authorized};
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    #[test]
    fn ws_auth_allows_when_token_not_configured_or_blank() {
        assert!(is_ws_authorized(None, None, &HashMap::new()));
        assert!(is_ws_authorized(Some("   "), None, &HashMap::new()));
    }

    #[test]
    fn ws_auth_accepts_bearer_and_query_tokens() {
        let mut query = HashMap::new();
        query.insert("access_token".to_string(), "secret".to_string());

        assert!(is_ws_authorized(
            Some("secret"),
            Some("Bearer secret"),
            &HashMap::new()
        ));
        assert!(is_ws_authorized(Some("secret"), None, &query));
    }

    #[test]
    fn ws_auth_scheme_is_case_insensitive() {
        // RFC 7235: scheme is case-insensitive.
        assert!(is_ws_authorized(
            Some("secret"),
            Some("bearer secret"),
            &HashMap::new()
        ));
        assert!(is_ws_authorized(
            Some("secret"),
            Some("BEARER secret"),
            &HashMap::new()
        ));
        assert!(is_ws_authorized(
            Some("secret"),
            Some("TOKEN secret"),
            &HashMap::new()
        ));
        assert!(is_ws_authorized(
            Some("secret"),
            Some("token secret"),
            &HashMap::new()
        ));
        // Unknown scheme is rejected even if it looks like a token.
        assert!(!is_ws_authorized(
            Some("secret"),
            Some("Basic secret"),
            &HashMap::new()
        ));
        // Empty token after scheme is rejected.
        assert!(!is_ws_authorized(
            Some("secret"),
            Some("Bearer "),
            &HashMap::new()
        ));
        // Bare token without scheme still accepted (OneBot compatibility).
        assert!(is_ws_authorized(
            Some("secret"),
            Some("secret"),
            &HashMap::new()
        ));
    }

    #[test]
    fn ws_auth_rejects_invalid_token() {
        let mut query = HashMap::new();
        query.insert("access_token".to_string(), "wrong".to_string());

        assert!(!is_ws_authorized(
            Some("secret"),
            Some("Bearer wrong"),
            &query
        ));
        assert!(!is_ws_authorized(Some("secret"), None, &HashMap::new()));
    }

    #[test]
    fn http_command_auth_allows_loopback_without_token() {
        let remote = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 12345);
        assert!(is_http_command_authorized(
            None,
            None,
            &HashMap::new(),
            Some(remote)
        ));
    }

    #[test]
    fn http_command_auth_requires_token_for_remote_when_not_loopback() {
        let remote = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10)), 12345);
        assert!(!is_http_command_authorized(
            None,
            None,
            &HashMap::new(),
            Some(remote)
        ));
        assert!(is_http_command_authorized(
            Some("secret"),
            Some("Bearer secret"),
            &HashMap::new(),
            Some(remote)
        ));
    }
}
