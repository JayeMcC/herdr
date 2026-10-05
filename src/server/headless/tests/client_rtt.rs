use super::*;

fn read_all_server_messages(bytes: Vec<u8>) -> Vec<ServerMessage> {
    let len = bytes.len() as u64;
    let mut cursor = std::io::Cursor::new(bytes);
    let mut messages = Vec::new();
    while cursor.position() < len {
        messages.push(protocol::read_message(&mut cursor, MAX_FRAME_SIZE).expect("decode"));
    }
    messages
}

fn rtt_echo(message: &ServerMessage) -> Option<u64> {
    match message {
        ServerMessage::EndpointControl { kind, data }
            if kind == crate::protocol::endpoint::CLIENT_RTT_ECHO_KIND =>
        {
            data.parse().ok()
        }
        _ => None,
    }
}

#[tokio::test]
async fn client_rtt_echo_leads_the_next_frame_in_one_render_item() {
    let mut server = test_headless_server();
    let pane_id = install_shared_view_test_runtime(&mut server);
    let (control, render) = connect_matching_test_shell(&mut server, 7);
    let _ = control.recv().expect("snapshot");
    server.render_and_stream();
    let _ = recv_pane_surface(&render, "initial surface");

    assert!(!server.handle_server_event(ServerEvent::ClientRttMark {
        client_id: 7,
        seq: 41,
    }));
    write_shared_test_pane(&mut server, pane_id, b"typed");
    server.render_and_stream();

    let messages = read_all_server_messages(render.recv().expect("frame after mark"));
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert_eq!(rtt_echo(&messages[0]), Some(41));
    assert!(matches!(
        messages[1],
        ServerMessage::PaneSurface(_) | ServerMessage::PaneSurfacePatch(_)
    ));

    // An echo is sent once; the following frame carries no stale echo.
    write_shared_test_pane(&mut server, pane_id, b" more");
    server.render_and_stream();
    let messages = read_all_server_messages(render.recv().expect("later frame"));
    assert!(messages.iter().all(|message| rtt_echo(message).is_none()));
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn client_rtt_client_without_marks_never_receives_an_echo() {
    let mut server = test_headless_server();
    let pane_id = install_shared_view_test_runtime(&mut server);
    let (control, render) = connect_matching_test_shell(&mut server, 7);
    let _ = control.recv().expect("snapshot");
    server.render_and_stream();
    let _ = recv_pane_surface(&render, "initial surface");

    for bytes in [b"a".as_slice(), b"b", b"c"] {
        write_shared_test_pane(&mut server, pane_id, bytes);
        server.render_and_stream();
        let messages = read_all_server_messages(render.recv().expect("frame"));
        assert_eq!(
            messages.len(),
            1,
            "old client gets frames only: {messages:?}"
        );
    }
    while let Ok(bytes) = control.try_recv() {
        assert!(read_all_server_messages(bytes)
            .iter()
            .all(|message| rtt_echo(message).is_none()));
    }
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn client_rtt_marks_never_force_a_render_and_keep_the_highest_sequence() {
    let mut server = test_headless_server();
    let pane_id = install_shared_view_test_runtime(&mut server);
    let (control, render) = connect_matching_test_shell(&mut server, 7);
    let _ = control.recv().expect("snapshot");
    server.render_and_stream();
    let _ = recv_pane_surface(&render, "initial surface");

    for seq in [3, 9, 5] {
        assert!(!server.handle_server_event(ServerEvent::ClientRttMark { client_id: 7, seq }));
    }
    assert!(!server.handle_server_event(ServerEvent::ClientRttMark {
        client_id: 99,
        seq: 1,
    }));
    write_shared_test_pane(&mut server, pane_id, b"x");
    server.render_and_stream();
    let messages = read_all_server_messages(render.recv().expect("frame"));
    assert_eq!(rtt_echo(&messages[0]), Some(9));
    shutdown_test_runtimes(&mut server);
}
