use hmac::{Hmac, Mac};
use http::HeaderMap;
use sha2::Sha256;

pub fn token_is_valid(token: &str) -> bool {
    !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_graphic())
}

pub fn bearer_matches(headers: &HeaderMap, expected: &str) -> bool {
    let mut values = headers.get_all(http::header::AUTHORIZATION).iter();
    let Some(value) = values.next().and_then(|value| value.to_str().ok()) else {
        return false;
    };
    if values.next().is_some() || expected.is_empty() {
        return false;
    }
    let Some((scheme, provided)) = value.split_once(' ') else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("Bearer")
        || provided.is_empty()
        || provided.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return false;
    }
    let Ok(mut expected_mac) = Hmac::<Sha256>::new_from_slice(expected.as_bytes()) else {
        return false;
    };
    expected_mac.update(expected.as_bytes());
    let tag = expected_mac.finalize().into_bytes();
    let Ok(mut provided_mac) = Hmac::<Sha256>::new_from_slice(expected.as_bytes()) else {
        return false;
    };
    provided_mac.update(provided.as_bytes());
    provided_mac.verify_slice(&tag).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::header::{HeaderValue, AUTHORIZATION};

    #[test]
    fn tokens_require_nonempty_ascii_graphic_characters() {
        for token in [
            "",
            " secret",
            "secret ",
            "secret token",
            "secret\ttoken",
            "secret\ntoken",
            "secret\rtoken",
            "secret\0token",
            "secret\u{1f}token",
            "secret\u{7f}token",
            "secret令牌",
            "secret\u{a0}token",
        ] {
            assert!(!token_is_valid(token), "accepted invalid token: {token:?}");
        }
        for token in [
            "a",
            "secret-token",
            "AZaz09-._~+/=!@#$%^&*()[]{}:;\"'<>?,\\|",
        ] {
            assert!(token_is_valid(token), "rejected valid token: {token:?}");
        }
        for byte in 0..=127_u8 {
            let token = format!("prefix{}suffix", char::from(byte));
            assert_eq!(token_is_valid(&token), (33..=126).contains(&byte));
        }
    }

    #[test]
    fn bearer_requires_one_nonempty_token_and_the_correct_scheme() {
        for (value, accepted) in [
            ("Bearer secret-token", true),
            ("bearer secret-token", true),
            ("BEARER secret-token", true),
            ("secret-token", false),
            ("Basic secret-token", false),
            ("Bearer wrong-token", false),
            ("Bearer secret-token-longer", false),
            ("Bearer secret", false),
            ("Bearer ", false),
            ("", false),
            ("Bearer  secret-token", false),
            ("Bearer secret-token ", false),
            ("Bearer\tsecret-token", false),
            ("Bearer secret-token, Bearer secret-token", false),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(AUTHORIZATION, HeaderValue::from_str(value).unwrap());
            assert_eq!(
                bearer_matches(&headers, "secret-token"),
                accepted,
                "{value:?}"
            );
            assert!(!bearer_matches(&headers, ""));
        }
        assert!(!bearer_matches(&HeaderMap::new(), "secret-token"));
    }

    #[test]
    fn bearer_rejects_duplicate_and_non_text_headers() {
        let mut headers = HeaderMap::new();
        headers.append(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer secret-token"),
        );
        headers.append(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer secret-token"),
        );
        assert!(!bearer_matches(&headers, "secret-token"));

        headers.clear();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_bytes(b"Bearer \xff").unwrap(),
        );
        assert!(!bearer_matches(&headers, "secret-token"));
    }
}
