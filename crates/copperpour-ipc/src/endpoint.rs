/// Remove credentials, query values, and fragments before an IPC endpoint is
/// reported in diagnostics.
pub fn redact_endpoint(endpoint: &str) -> String {
    let (without_fragment, had_fragment) = endpoint
        .split_once('#')
        .map_or((endpoint, false), |(head, _)| (head, true));
    let (without_query, had_query) = without_fragment
        .split_once('?')
        .map_or((without_fragment, false), |(head, _)| (head, true));

    let without_credentials = if let Some((scheme, rest)) = without_query.split_once("://") {
        if let Some((_, authority_and_path)) = rest.split_once('@') {
            format!("{scheme}://[redacted]@{authority_and_path}")
        } else {
            without_query.to_string()
        }
    } else {
        without_query.to_string()
    };

    if had_query || had_fragment {
        format!("{without_credentials} [query/fragment redacted]")
    } else {
        without_credentials
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_query_and_fragment_are_redacted() {
        assert_eq!(
            redact_endpoint("tcp://user:secret@127.0.0.1:9000/api?token=hidden#detail"),
            "tcp://[redacted]@127.0.0.1:9000/api [query/fragment redacted]"
        );
        assert_eq!(
            redact_endpoint("ipc:///tmp/kicad/api.sock"),
            "ipc:///tmp/kicad/api.sock"
        );
    }
}
