use super::*;
use std::cell::Cell;
use std::rc::Rc;

#[test]
fn long_attachment_name_keeps_extension() {
    let long = format!("{}.pdf", "Faktura-VAT-".repeat(30));
    let name = sanitize_filename(&long);
    assert_eq!(name.len(), 180);
    assert!(name.ends_with(".pdf"), "{name}");
    assert!(name.starts_with("Faktura-VAT-"));
    let upper = sanitize_filename(&format!("{}.PDF", "x".repeat(400)));
    assert_eq!(upper.len(), 180);
    assert!(upper.ends_with("x.PDF"), "{upper}");
    // Without a usable extension the old cut applies.
    assert_eq!(sanitize_filename(&"y".repeat(300)), "y".repeat(180));

    // The planned file is still a mail candidate.
    let msg = serde_json::json!({
        "payload": {"parts": [{
            "filename": long,
            "mimeType": "application/pdf",
            "body": {"attachmentId": "att-0"}
        }]}
    });
    let plan = gmail_attachment_plan("18c2f3a4", &msg["payload"], &HashSet::from(["pdf".into()]));
    assert_eq!(plan.len(), 1);
    assert!(plan[0].file_name.starts_with("18c2f3a4_1_Faktura-VAT-"));
    assert!(plan[0].file_name.ends_with(".pdf"));
    assert!(is_mail_candidate_file(Path::new(&plan[0].file_name)));
}

#[test]
fn short_names_are_unchanged() {
    for (input, expected) in [
        ("x.pdf", "x.pdf"),
        ("Faktura VAT 1/2026.pdf", "Faktura_VAT_1_2026.pdf"),
        ("_zażółć.PDF_", "za____.PDF"),
        ("18c2f3a4b5d6e7f8", "18c2f3a4b5d6e7f8"),
    ] {
        assert_eq!(sanitize_filename(input), expected, "{input}");
    }
    let exactly = format!("{}.pdf", "a".repeat(176));
    assert_eq!(sanitize_filename(&exactly), exactly);
    // One character more: the stem is cut, the extension stays.
    let over = format!("{}.pdf", "a".repeat(177));
    assert_eq!(sanitize_filename(&over), exactly);
}

#[test]
fn pkce_challenge_matches_rfc7636_appendix_b() {
    assert_eq!(
        pkce_code_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn state_and_verifier_are_full_length_random() {
    let (a, b) = (oauth_state().unwrap(), oauth_state().unwrap());
    assert_eq!(a.len(), 43, "32 bytes in base64url");
    assert_eq!(URL_SAFE_NO_PAD.decode(&a).unwrap().len(), 32);
    assert_ne!(a, b);
    let (v1, v2) = (pkce_code_verifier().unwrap(), pkce_code_verifier().unwrap());
    assert_eq!(v1.len(), 43);
    assert_ne!(v1, v2);
    assert!(
        v1.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    );
}

#[test]
fn oauth_requests_are_classified() {
    let state = "S1";
    assert_eq!(
        classify_oauth_request("GET /favicon.ico HTTP/1.1", state),
        OAuthRequest::NotFound
    );
    assert_eq!(classify_oauth_request("", state), OAuthRequest::NotFound);
    assert_eq!(
        classify_oauth_request("POST /callback?code=c&state=S1 HTTP/1.1", state),
        OAuthRequest::NotFound
    );
    assert_eq!(
        classify_oauth_request("GET /callbackx?code=c&state=S1 HTTP/1.1", state),
        OAuthRequest::NotFound
    );
    assert_eq!(
        classify_oauth_request("GET /callback?code=c&state=other HTTP/1.1", state),
        OAuthRequest::BadState
    );
    assert_eq!(
        classify_oauth_request("GET /callback?code=c HTTP/1.1", state),
        OAuthRequest::BadState
    );
    // An error without our state could be forged; it must not end the flow.
    assert_eq!(
        classify_oauth_request("GET /callback?error=access_denied HTTP/1.1", state),
        OAuthRequest::BadState
    );
    assert!(matches!(
        classify_oauth_request("GET /callback?error=access_denied&state=S1 HTTP/1.1", state),
        OAuthRequest::Failed(message) if message.contains("access_denied")
    ));
    assert!(matches!(
        classify_oauth_request("GET /callback?state=S1 HTTP/1.1", state),
        OAuthRequest::Failed(_)
    ));
    assert_eq!(
        classify_oauth_request("GET /callback?state=S1&code=4%2F0Ab HTTP/1.1", state),
        OAuthRequest::Code("4/0Ab".into())
    );
}

#[test]
fn listener_ignores_stray_requests_until_the_callback() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let client = std::thread::spawn(move || {
        let mut statuses = Vec::new();
        for request in [
            None,
            Some("GET /favicon.ico HTTP/1.1\r\nHost: x\r\n\r\n"),
            Some("GET /callback?code=forged&state=wrong HTTP/1.1\r\n\r\n"),
            Some("GET /callback?code=good&state=S1 HTTP/1.1\r\n\r\n"),
        ] {
            let mut stream = std::net::TcpStream::connect(addr).unwrap();
            let Some(request) = request else {
                continue; // port probe: connect and close
            };
            stream.write_all(request.as_bytes()).unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            statuses.push(response.lines().next().unwrap_or_default().to_string());
        }
        statuses
    });
    let code = wait_for_oauth_code(&listener, "S1", Duration::from_secs(20)).unwrap();
    assert_eq!(code, "good");
    assert_eq!(
        client.join().unwrap(),
        [
            "HTTP/1.1 404 Not Found",
            "HTTP/1.1 400 Bad Request",
            "HTTP/1.1 200 OK"
        ]
    );
}

#[test]
fn listener_gives_up_after_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let err = wait_for_oauth_code(&listener, "S1", Duration::from_millis(200)).unwrap_err();
    assert!(err.to_string().contains("lab onboard"), "{err}");
}

fn stored_token(expires_at: Option<DateTime<Utc>>, refresh: bool) -> GmailTokenFile {
    GmailTokenFile {
        access_token: "old".into(),
        refresh_token: refresh.then(|| "refresh".to_string()),
        token_type: None,
        expires_at,
        scope: None,
    }
}

#[test]
fn unknown_expiry_is_refreshed_when_possible() {
    let now = Utc::now();
    assert!(gmail_token_needs_refresh(&stored_token(None, true), now));
    assert!(!gmail_token_needs_refresh(&stored_token(None, false), now));
    let soon = Some(now + chrono::Duration::seconds(30));
    assert!(gmail_token_needs_refresh(&stored_token(soon, true), now));
    // Known to be expired without a refresh token: fail early with the onboarding hint.
    assert!(gmail_token_needs_refresh(&stored_token(soon, false), now));
    let later = Some(now + chrono::Duration::minutes(30));
    assert!(!gmail_token_needs_refresh(&stored_token(later, true), now));
}

fn counting_token(expires_at: Option<DateTime<Utc>>) -> (GmailAccessToken, Rc<Cell<usize>>) {
    let refreshes = Rc::new(Cell::new(0));
    let counter = refreshes.clone();
    let token = GmailAccessToken {
        token: std::cell::RefCell::new(stored_token(expires_at, true)),
        refresher: Some(Box::new(move |previous| {
            counter.set(counter.get() + 1);
            assert_eq!(previous.refresh_token.as_deref(), Some("refresh"));
            Ok(GmailTokenFile {
                access_token: format!("new{}", counter.get()),
                expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
                ..previous.clone()
            })
        })),
    };
    (token, refreshes)
}

#[test]
fn unauthorized_refreshes_once_and_retries() {
    let (token, refreshes) = counting_token(Some(Utc::now() + chrono::Duration::hours(1)));
    let mut seen = Vec::new();
    let value = gmail_call_with_refresh(&token, |access| {
        seen.push(access.to_string());
        Ok((access != "old").then_some(7))
    })
    .unwrap();
    assert_eq!(value, 7);
    assert_eq!(seen, ["old", "new1"]);
    assert_eq!(refreshes.get(), 1);
    // The refreshed token is used for later requests.
    assert_eq!(token.current().unwrap(), "new1");
}

#[test]
fn repeated_unauthorized_is_an_error_not_a_loop() {
    let (token, refreshes) = counting_token(Some(Utc::now() + chrono::Duration::hours(1)));
    let mut calls = 0;
    let err = gmail_call_with_refresh(&token, |_| {
        calls += 1;
        Ok(None::<()>)
    })
    .unwrap_err();
    assert_eq!(calls, 2);
    assert_eq!(refreshes.get(), 1);
    assert!(err.to_string().contains("401"), "{err}");

    // An environment token cannot be refreshed: one call, then a clear error.
    let fixed = GmailAccessToken::fixed("env".into());
    let mut calls = 0;
    let err = gmail_call_with_refresh(&fixed, |_| {
        calls += 1;
        Ok(None::<()>)
    })
    .unwrap_err();
    assert_eq!(calls, 1);
    assert!(err.to_string().contains("401"), "{err}");
}

#[test]
fn token_close_to_expiry_is_refreshed_before_the_request() {
    let (token, refreshes) = counting_token(Some(Utc::now() + chrono::Duration::seconds(20)));
    let seen = gmail_call_with_refresh(&token, |access| Ok(Some(access.to_string()))).unwrap();
    assert_eq!(seen, "new1");
    assert_eq!(refreshes.get(), 1);
    gmail_call_with_refresh(&token, |access| Ok(Some(access.to_string()))).unwrap();
    assert_eq!(refreshes.get(), 1, "fresh token is not refreshed again");
}

#[test]
fn failed_refresh_points_to_onboarding() {
    for (status, body) in [
        (
            400,
            r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#,
        ),
        (401, r#"{"error":"unauthorized_client"}"#),
        (400, "not json"),
    ] {
        let message = gmail_refresh_error(status, body).to_string();
        assert!(
            message.contains("wygasła albo została cofnięta"),
            "{message}"
        );
        assert!(message.contains("lab onboard"), "{message}");
        assert!(
            !message.contains("expired or revoked"),
            "body not echoed: {message}"
        );
    }
    assert!(
        gmail_refresh_error(400, r#"{"error":"invalid_grant"}"#)
            .to_string()
            .contains("invalid_grant")
    );
    let server = gmail_refresh_error(503, "").to_string();
    assert!(server.contains("HTTP 503"), "{server}");
    assert!(!server.contains("cofnięta"), "{server}");
}

#[test]
fn year_window_runs_from_december_before_to_february_after() {
    assert_eq!(
        gmail_year_window(2026),
        "after:2025/12/01 before:2027/02/01"
    );
    assert_eq!(
        gmail_year_window(2000),
        "after:1999/12/01 before:2001/02/01"
    );
    // Consecutive years overlap by December and January, never leave a gap.
    for query in [default_gmail_query(2026), amazon_gmail_query(2026)] {
        assert!(
            query.starts_with("after:2025/12/01 before:2027/02/01 "),
            "{query}"
        );
    }
}

struct SweepDir(PathBuf);

impl Drop for SweepDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn stale_temp_files_are_swept_and_young_ones_kept() {
    use std::time::SystemTime;
    let dir = SweepDir(std::env::temp_dir().join(format!(
        "lab-gmail-sweep-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap()
    )));
    fs::create_dir(&dir.0).unwrap();
    let now = SystemTime::now();
    let two_hours_ago = now - Duration::from_secs(2 * 60 * 60);
    let write = |name: &str, modified: SystemTime| {
        let path = dir.0.join(name);
        fs::write(&path, b"x").unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        path
    };
    let old_pdf_tmp = write(".msg1_1_a.pdf.lab-tmp-a1b2c3", two_hours_ago);
    let old_jsonl_tmp = write(".records.jsonl.lab-tmp-12345", two_hours_ago);
    let young_tmp = write(
        ".msg2_1_b.pdf.lab-tmp-d4e5f6",
        now - Duration::from_secs(59 * 60),
    );
    let old_attachment = write("msg1_1_a.pdf", two_hours_ago);
    let old_hidden = write(".DS_Store", two_hours_ago);
    let not_temp = write("x.lab-tmp-name.pdf", two_hours_ago);
    let nested = dir.0.join("nested");
    fs::create_dir(&nested).unwrap();
    let nested_tmp = nested.join(".c.pdf.lab-tmp-777");
    fs::write(&nested_tmp, b"x").unwrap();
    fs::File::options()
        .write(true)
        .open(&nested_tmp)
        .unwrap()
        .set_modified(two_hours_ago)
        .unwrap();

    assert_eq!(remove_stale_temp_files(&dir.0, now), 2);
    assert!(!old_pdf_tmp.exists());
    assert!(!old_jsonl_tmp.exists());
    for kept in [
        &young_tmp,
        &old_attachment,
        &old_hidden,
        &not_temp,
        &nested_tmp,
    ] {
        assert!(kept.exists(), "{}", kept.display());
    }
    // An hour later the young one goes too; a missing folder is not an error.
    assert_eq!(
        remove_stale_temp_files(&dir.0, now + Duration::from_secs(60 * 60)),
        1
    );
    assert!(!young_tmp.exists());
    assert_eq!(remove_stale_temp_files(&dir.0.join("absent"), now), 0);
}

#[cfg(unix)]
#[test]
fn temp_sweep_does_not_follow_symlinks() {
    let dir = SweepDir(std::env::temp_dir().join(format!(
        "lab-gmail-sweep-link-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap()
    )));
    fs::create_dir(&dir.0).unwrap();
    let target = dir.0.join("target.pdf");
    fs::write(&target, b"keep").unwrap();
    let link = dir.0.join(".target.pdf.lab-tmp-link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let later = std::time::SystemTime::now() + Duration::from_secs(3 * 60 * 60);
    assert_eq!(remove_stale_temp_files(&dir.0, later), 0);
    assert!(target.is_file());
    assert!(link.symlink_metadata().is_ok());
}

#[test]
fn failed_code_exchange_names_the_step_and_codes_without_the_body() {
    let message = gmail_code_exchange_error(
        400,
        r#"{"error":"invalid_grant","error_description":"Code was already redeemed.","secret_hint":"do-not-echo"}"#,
    )
    .to_string();
    assert!(
        message.contains("wymianę kodu autoryzacji Gmail"),
        "{message}"
    );
    assert!(message.contains("HTTP 400"), "{message}");
    assert!(
        message.contains("invalid_grant: Code was already redeemed."),
        "{message}"
    );
    assert!(message.contains("`lab onboard` (krok Gmail)"), "{message}");
    assert!(!message.contains("do-not-echo"), "{message}");
    assert!(!message.contains('{'), "{message}");

    let code_only = gmail_code_exchange_error(401, r#"{"error":"invalid_client"}"#).to_string();
    assert!(
        code_only.contains("HTTP 401, invalid_client)"),
        "{code_only}"
    );

    // Not JSON, an odd code or a long or multi-line description: nothing echoed.
    for body in [
        "<html>upstream error token=abc</html>",
        r#"{"error":"bad code!","error_description":"x"}"#,
    ] {
        let message = gmail_code_exchange_error(502, body).to_string();
        assert!(message.contains("(HTTP 502)"), "{message}");
        assert!(message.contains("lab onboard"), "{message}");
    }
    let long = format!(
        r#"{{"error":"invalid_request","error_description":"{}"}}"#,
        "a".repeat(200)
    );
    let multi_line = r#"{"error":"invalid_request","error_description":"one\ntwo"}"#;
    for body in [long.as_str(), multi_line] {
        let message = gmail_code_exchange_error(400, body).to_string();
        assert!(message.contains("(HTTP 400, invalid_request)"), "{message}");
    }
}

#[test]
fn browser_for_oauth_gets_no_lab_secrets() {
    let command = oauth_browser_command("https://accounts.example.test/auth?x=1");
    assert_eq!(command.get_program(), "open");
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        ["https://accounts.example.test/auth?x=1"]
    );
    let removed = command
        .get_envs()
        .filter(|(_, value)| value.is_none())
        .map(|(key, _)| key.to_string_lossy().into_owned())
        .collect::<HashSet<_>>();
    for key in crate::SECRET_ENV_KEYS
        .iter()
        .chain(crate::hardening::CHILD_SECRET_ENV_KEYS.iter())
    {
        assert!(removed.contains(*key), "{key} not removed");
    }
}
