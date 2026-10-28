use kuggis::{
    ssh::{Invocation, Request},
    upload::{Options, Source},
    Workspace,
};

fn parse(args: &[&str]) -> kuggis::Result<Options> {
    Options::parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
}

#[test]
fn basename_explicit_destination_and_options_in_any_order() {
    let input = parse(&["/somewhere/a b.png", "--chat", "vt"]).unwrap();
    assert_eq!(
        input.source,
        Source::File {
            local: "/somewhere/a b.png".into(),
            destination: "a b.png".to_owned().try_into().unwrap()
        }
    );
    assert_eq!(input.chat.unwrap().as_str(), "vt");
    let input = parse(&["--replace", "--chat", "default", "a.png", "inputs/b.png"]).unwrap();
    assert!(input.replace);
    assert!(
        matches!(input.source, Source::File { destination, .. } if destination.as_str() == "inputs/b.png")
    );
    let input = parse(&["--", "-file.png"]).unwrap();
    assert!(
        matches!(input.source, Source::File { destination, .. } if destination.as_str() == "-file.png")
    );
    let input = parse(&["--chat", "vt", "--paste"]).unwrap();
    assert_eq!(input.source, Source::Clipboard);
}

#[test]
fn invalid_uploads_fail_before_clipboard_or_ssh_effects() {
    for args in [
        vec![],
        vec!["--paste", "file.png"],
        vec!["x", "y", "z"],
        vec!["--paste", "--paste"],
        vec!["x", "--replace", "--replace"],
        vec!["x", "--chat"],
        vec!["x", "--chat", "../bad"],
        vec!["x", "--chat", "a", "--chat", "b"],
        vec!["/"],
        vec!["x", "../escape"],
        vec!["bad\nfile"],
        vec!["x", "--bad"],
    ] {
        assert!(parse(&args).is_err(), "{args:?}");
    }
}

#[test]
fn chat_transfer_uses_one_nonterminal_request_with_quoted_path() {
    let workspace = Workspace::parse(include_str!("../../../examples/workspace.json")).unwrap();
    let request = Request::PutInChat {
        path: "a ' $(bad).png".to_owned().try_into().unwrap(),
        chat: "vt".to_owned().try_into().unwrap(),
        replace: true,
    };
    let invocation = Invocation::for_request(&workspace, &request);
    assert_eq!(invocation.args[0], "-T");
    let remote = invocation.args.last().unwrap();
    assert!(remote.contains("'--chat' 'vt' '--replace' '--'"));
    assert!(remote.ends_with("'a '\"'\"' $(bad).png'"));
}
