use bytes::Bytes;
use muse_core::{read_be_frame, write_be_frame, TunnelMessage, DEFAULT_MAX_FRAME_SIZE};
use std::io::Cursor;

#[tokio::test]
async fn test_be_frame_roundtrip() {
    let data = b"{\"message\": \"hello space inference\"}";
    let mut cursor = Cursor::new(Vec::new());

    write_be_frame(&mut cursor, data).await.unwrap();
    cursor.set_position(0);

    let read_back = read_be_frame(&mut cursor, DEFAULT_MAX_FRAME_SIZE)
        .await
        .unwrap();
    assert_eq!(read_back, data);
}

#[test]
fn test_tunnel_message_encoding() {
    let msg = TunnelMessage::Register {
        vm_id: "VM1".to_string(),
        token: "secret123".to_string(),
        is_fresh_boot: true,
    };
    let encoded = msg.encode().unwrap();
    let decoded = TunnelMessage::decode(Bytes::from(encoded)).unwrap();

    match decoded {
        TunnelMessage::Register {
            vm_id,
            token,
            is_fresh_boot,
        } => {
            assert_eq!(vm_id, "VM1");
            assert_eq!(token, "secret123");
            assert!(is_fresh_boot);
        }
        _ => panic!("Unexpected message variant"),
    }
}
