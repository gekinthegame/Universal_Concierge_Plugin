#[cfg(test)]
mod tests {
    use super::{envelope_declares_room, forbidden_security_flag, retrieve_query, HELP};

    #[test]
    fn inbound_envelope_must_declare_the_subscribed_room() {
        let envelope = r#"{
            "id":"room-a",
            "payload":"hello",
            "next":[],
            "refs":[],
            "clock":1,
            "key":"author",
            "sig":"signature"
        }"#;
        assert!(envelope_declares_room(envelope, "room-a"));
        assert!(!envelope_declares_room(envelope, "room-b"));
        assert!(!envelope_declares_room("not-json", "room-a"));
    }

    #[test]
    fn cli_exposes_no_password_or_force_bypass() {
        assert!(HELP.contains("share-private"));
        assert!(!HELP.contains("--password"));
        assert!(!HELP.contains("--force"));
        assert!(!HELP.contains("unlock"));
        assert_eq!(
            forbidden_security_flag(&["publish-public".into(), "--password=secret".into()]),
            Some("--password=secret")
        );
        assert_eq!(
            forbidden_security_flag(&["export-car".into(), "--force".into()]),
            Some("--force")
        );
    }

    #[test]
    fn retrieve_query_skips_boolean_and_value_flags() {
        let args = vec![
            "retrieve".to_string(),
            "rust".to_string(),
            "--standalone".to_string(),
            "ownership".to_string(),
            "--budget".to_string(),
            "2000".to_string(),
            "--external".to_string(),
            "--kind".to_string(),
            "memory,decision".to_string(),
        ];
        assert_eq!(retrieve_query(&args), "rust ownership");
    }
}
