//! 端到端 wire 测试：连真实容器 server socket 调 `server.env`，验证 server
//! 运行时注入项（XAUTHORITY 等）可经协议读回。
//!
//! 需 `EASYTIDY_TEST_SOCK=<容器 server.sock 路径>`（如
//! /run/user/1000/easytidy/chrome-*/server.sock）且容器在运行；未设则跳过
//! （CI 无容器时安全跳过）。用真实 FrameCodec（Sink<Frame>/Stream<Item=Frame>），
//! 无手写分帧歧义。

use easytidy_protocol::{
    Frame, FrameCodec, Handshake, Message, MsgKind, PROTOCOL_VERSION, ServerEnvResp,
};
use futures::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio_util::codec::Framed;

async fn call_server_env(sock: &str) -> ServerEnvResp {
    let stream = UnixStream::connect(sock).await.expect("connect");
    let mut framed = Framed::new(stream, FrameCodec::new());

    // 握手
    framed
        .send(Frame::Json(Message {
            id: 1,
            kind: MsgKind::Req,
            op: "hello".to_string(),
            payload: serde_json::to_value(Handshake {
                v: PROTOCOL_VERSION,
                client: "wire-env-test".to_string(),
                wants: vec!["apps".to_string()],
            })
            .unwrap(),
            err: None,
        }))
        .await
        .unwrap();
    let ack = read_json_frame(&mut framed).await.expect("hello ack");
    assert_eq!(ack.op, "hello");

    // server.env
    framed
        .send(Frame::Json(Message {
            id: 2,
            kind: MsgKind::Req,
            op: "server.env".to_string(),
            payload: serde_json::json!(null),
            err: None,
        }))
        .await
        .unwrap();
    let resp = read_json_frame(&mut framed).await.expect("server.env resp");
    assert_eq!(resp.op, "server.env");
    serde_json::from_value(resp.payload).expect("parse ServerEnvResp")
}

async fn read_json_frame(framed: &mut Framed<UnixStream, FrameCodec>) -> Option<Message> {
    loop {
        let f = framed.next().await.expect("read").expect("frame present");
        if let Frame::Json(m) = f {
            return Some(m);
        }
    }
}

#[tokio::test]
async fn wire_server_env_returns_injected() {
    let Some(sock) = std::env::var("EASYTIDY_TEST_SOCK").ok() else {
        eprintln!("skip: EASYTIDY_TEST_SOCK 未设");
        return;
    };
    let resp = call_server_env(&sock).await;
    eprintln!("server.env → {:?}", resp.env);
    // GUI 容器（chrome）应至少探测到 XAUTHORITY（Xwayland auth 文件存在）
    assert!(
        resp.env.iter().any(|e| e.key == "XAUTHORITY"),
        "GUI 容器应注入 XAUTHORITY，实际: {:?}",
        resp.env
    );
}
