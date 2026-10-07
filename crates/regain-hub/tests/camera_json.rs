//! Pure JSON streams and private loopback peers. No equipment discovery or I/O.
use regain_hub::{
    camera::{
        image::{
            ElementType, IMAGE_CHUNK_BYTES, ImageBudget, ImageDescriptor, ImageOrder,
            ImageReadError,
        },
        json_image::read_json_image,
    },
    source::ErrorKind,
};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

fn envelope(kind: ElementType, values: Vec<Value>) -> Vec<u8> {
    serde_json::to_vec(&json!({"Value":[[values[0],values[1]],[values[2],values[3]],[values[4],values[5]]],"Rank":2,"Type":kind as u32,"ErrorNumber":0,"ClientTransactionID":17})).unwrap()
}
async fn rejects(body: &[u8]) {
    let budget = ImageBudget::new(body.len() + 4096).unwrap();
    let result = read_json_image(&mut &body[..], &budget, 17).await;
    assert!(
        matches!(result, Err(ImageReadError::Contract(_))),
        "Malformed response accepted or misclassified"
    );
    assert_eq!(budget.used_bytes(), 0);
}

#[tokio::test]
async fn all_numeric_types_preserve_extremes_little_endian_order_and_pixel_lifetime() {
    let mut cases = Vec::new();
    macro_rules! case {
        ($kind:ident,$ty:ty,$values:expr) => {{
            let values: [$ty; 6] = $values;
            cases.push((
                ElementType::$kind,
                values.iter().map(|v| json!(*v)).collect::<Vec<_>>(),
                values
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect::<Vec<_>>(),
            ));
        }};
    }
    case!(Int16, i16, [i16::MIN, 0, i16::MAX, -1, 42, 4096]);
    case!(Int32, i32, [i32::MIN, 0, i32::MAX, -1, 65535, 50000]);
    case!(Int64, i64, [i64::MIN, 0, i64::MAX, -1, 65535, 50000]);
    case!(UInt16, u16, [0, 1, u16::MAX, 32768, 42, 50000]);
    case!(UInt32, u32, [0, 1, u32::MAX, 2147483648, 65535, 50000]);
    case!(
        UInt64,
        u64,
        [0, 1, u64::MAX, 9223372036854775808, 65535, 50000]
    );
    case!(Byte, u8, [0, 1, u8::MAX, 128, 42, 17]);
    case!(
        Double,
        f64,
        [
            1.25,
            -0.0,
            f64::MAX,
            f64::MIN_POSITIVE,
            f64::from_bits(1),
            -f64::MAX
        ]
    );
    case!(
        Single,
        f32,
        [
            0.1,
            -0.0,
            f32::MAX,
            f32::MIN_POSITIVE,
            f32::from_bits(1),
            -f32::MAX
        ]
    );
    for (kind, values, expected) in cases {
        let body = envelope(kind, values);
        let budget = ImageBudget::new(body.len() + expected.len()).unwrap();
        let image = read_json_image(&mut body.as_slice(), &budget, 17)
            .await
            .unwrap();
        assert_eq!(image.descriptor().element_type(), kind);
        assert_eq!(image.descriptor().transmission_type(), kind);
        assert_eq!(image.descriptor().order(), ImageOrder::Ascom);
        assert_eq!(image.bytes(), expected, "{kind:?}");
        assert_eq!(budget.used_bytes(), expected.len());
        let pinned = image.clone();
        drop(image);
        assert_eq!(budget.used_bytes(), expected.len());
        drop(pinned);
        assert_eq!(budget.used_bytes(), 0);
    }
}

#[tokio::test]
async fn non_square_rank_three_and_one_plane_arrays_keep_their_declared_rank() {
    for planes in [1, 3, 4] {
        let mut values = Vec::new();
        let mut expected = Vec::new();
        for x in 0..3 {
            let mut column = Vec::new();
            for y in 0..2 {
                let mut pixel = Vec::new();
                for plane in 0..planes {
                    let value = x * 100 + y * 10 + plane;
                    pixel.push(value);
                    expected.extend_from_slice(&(value as i32).to_le_bytes());
                }
                column.push(pixel);
            }
            values.push(column);
        }
        let body = serde_json::to_vec(&json!({"Value":values,"Type":2,"Rank":3})).unwrap();
        let budget = ImageBudget::new(body.len() + expected.len()).unwrap();
        let image = read_json_image(&mut body.as_slice(), &budget, 17)
            .await
            .unwrap();
        assert_eq!(image.descriptor().planes(), Some(planes));
        assert_eq!(image.bytes(), expected);
        drop(image);
        assert_eq!(budget.used_bytes(), 0);
    }
}

#[tokio::test]
async fn finite_double_bit_patterns_round_trip_without_ulp_changes() {
    let mut state = 0x9e3779b97f4a7c15u64;
    let mut values = Vec::new();
    while values.len() < 4000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let value = f64::from_bits(state);
        if value.is_finite() {
            values.push(value);
        }
    }
    let body = serde_json::to_vec(&json!({"Value":[values],"Type":3,"Rank":2})).unwrap();
    let budget = ImageBudget::new(body.len() + values.len() * 8).unwrap();
    let image = read_json_image(&mut body.as_slice(), &budget, 17)
        .await
        .unwrap();
    for (index, (bytes, value)) in image
        .bytes()
        .as_chunks::<8>()
        .0
        .iter()
        .zip(&values)
        .enumerate()
    {
        assert_eq!(
            u64::from_le_bytes(*bytes),
            value.to_bits(),
            "Double pixel {index}"
        );
    }
}

#[tokio::test]
async fn malformed_envelopes_ragged_shapes_and_coerced_numbers_never_publish() {
    for body in [
        r#"{"Value":[[1,2]],"Type":2,"Rank":2,"Type":3}"#,
        r#"{"Value":[[1,2]],"Type":2,"Rank":2,"ErrorNumber":0,"ErrorNumber":1024}"#,
        r#"{"Value":[[1,2]],"Type":2,"Rank":2,"ClientTransactionID":17,"ClientTransactionID":18}"#,
        r#"{"Value":[[1,2]],"Value":[[3,4]],"Type":2,"Rank":2}"#,
        r#"{"Value":[[1,2]],"Type":2,"Rank":2,"ClientTransactionID":18}"#,
        r#"{"Value":[[1,2]],"Type":2,"Rank":2,"ClientTransactionID":null}"#,
        r#"{"Value":[[1,2]],"Type":2,"Rank":2,"ServerTransactionID":"1"}"#,
        r#"{"Value":[[1,2]],"Type":2,"Rank":2,"ErrorNumber":null}"#,
        r#"{"Value":[[1,2]],"Type":2,"Rank":2,"ErrorNumber":-1}"#,
        r#"{"Value":[[1,2]],"Type":2,"Rank":2,"ErrorMessage":"unknown failure"}"#,
        r#"{"Value":[[1,2]],"Type":2,"Rank":3}"#,
        r#"{"Value":[[1,2]],"Rank":2}"#,
        r#"{"Value":[[1,2]],"Type":2}"#,
        r#"{"Value":[[1,2]],"Type":0,"Rank":2}"#,
        r#"{"Value":[[1,2]],"Type":10,"Rank":2}"#,
        r#"{"Value":[[1,2]],"Type":2,"Rank":2}{}"#,
        r#"{"Value":[[1,2]],"Type":2,"Rank":2"#,
        r#"{"Value":[],"Type":2,"Rank":2}"#,
        r#"{"Value":[[]],"Type":2,"Rank":2}"#,
        r#"{"Value":[1,2],"Type":2,"Rank":2}"#,
        r#"{"Value":[[1],[2,3]],"Type":2,"Rank":2}"#,
        r#"{"Value":[[[1]],[[2,3]]],"Type":2,"Rank":3}"#,
        r#"{"Value":[[[[1]]]],"Type":2,"Rank":3}"#,
        r#"{"Value":[[null]],"Type":2,"Rank":2}"#,
        r#"{"Value":[[true]],"Type":2,"Rank":2}"#,
        r#"{"Value":[["1"]],"Type":2,"Rank":2}"#,
        r#"{"Value":[[1.0]],"Type":2,"Rank":2}"#,
        r#"{"Value":[[2147483648]],"Type":2,"Rank":2}"#,
        r#"{"Value":[[-32769]],"Type":1,"Rank":2}"#,
        r#"{"Value":[[65536]],"Type":8,"Rank":2}"#,
        r#"{"Value":[[-1]],"Type":5,"Rank":2}"#,
        r#"{"Value":[[18446744073709551616]],"Type":5,"Rank":2}"#,
        r#"{"Value":[[9223372036854775808]],"Type":7,"Rank":2}"#,
        r#"{"Value":[[1e100]],"Type":4,"Rank":2}"#,
        r#"{"Value":[[1e999]],"Type":3,"Rank":2}"#,
    ] {
        rejects(body.as_bytes()).await;
    }
}

#[tokio::test]
async fn errors_preserve_codes_and_remain_reportable_with_a_pinned_budget() {
    let budget = ImageBudget::new(1).unwrap();
    let pin = budget
        .allocate(
            ImageDescriptor::new(
                1,
                1,
                None,
                ElementType::Byte,
                ElementType::Byte,
                ImageOrder::Ascom,
            )
            .unwrap(),
        )
        .unwrap()
        .finish();
    for body in [
        r#"{"Value":null,"ErrorMessage":"private error λ","ErrorNumber":1024,"ClientTransactionID":17}"#,
        r#"{"ErrorNumber":1031,"ErrorMessage":"private error","Value":null}"#,
    ] {
        let result = read_json_image(&mut body.as_bytes(), &budget, 17).await;
        assert!(matches!(
            result,
            Err(ImageReadError::Upstream {
                code: 1024 | 1031,
                ..
            })
        ));
        assert_eq!(budget.used_bytes(), 1);
    }
    rejects(br#"{"ErrorNumber":1024,"ClientTransactionID":18}"#).await;
    let body =
        serde_json::to_vec(&json!({"ErrorNumber":1024,"ErrorMessage":"x".repeat(4097)})).unwrap();
    rejects(&body).await;
    drop(pin);
    assert_eq!(budget.used_bytes(), 0);
}

#[tokio::test]
async fn staging_and_pixels_compete_for_one_budget_without_evicting_old_readers() {
    let body = envelope(ElementType::Int32, (1..=6).map(|v| json!(v)).collect());
    for maximum in [body.len() - 1, body.len(), body.len() + 23] {
        let budget = ImageBudget::new(maximum).unwrap();
        let result = read_json_image(&mut body.as_slice(), &budget, 17).await;
        assert!(
            matches!(result,Err(ImageReadError::Contract(error)) if error.kind==ErrorKind::Busy)
        );
        assert_eq!(budget.used_bytes(), 0);
    }
    let budget = ImageBudget::new(body.len() + 24).unwrap();
    let image = read_json_image(&mut body.as_slice(), &budget, 17)
        .await
        .unwrap();
    let result = read_json_image(&mut body.as_slice(), &budget, 17).await;
    assert!(matches!(result,Err(ImageReadError::Contract(error)) if error.kind==ErrorKind::Busy));
    assert_eq!(image.bytes().len(), 24);
    assert_eq!(budget.used_bytes(), 24);
    drop(image);
    assert_eq!(budget.used_bytes(), 0);
}

#[tokio::test]
async fn chunked_json_larger_than_scalar_envelope_decodes_without_a_value_tree() {
    let height = 180000usize;
    let body = format!(
        "{{\"Value\":[[{}]],\"Type\":2,\"Rank\":2}}",
        vec!["65535"; height].join(",")
    )
    .into_bytes();
    assert!(body.len() > 1024 * 1024);
    let budget = ImageBudget::new(body.len() + height * 4).unwrap();
    let (mut output, mut input) = tokio::io::duplex(4096);
    let sender = tokio::spawn(async move {
        for chunk in body.chunks(4096) {
            output.write_all(chunk).await.unwrap();
        }
    });
    let image = read_json_image(&mut input, &budget, 17).await.unwrap();
    sender.await.unwrap();
    assert_eq!(image.descriptor().width(), 1);
    assert_eq!(image.descriptor().height(), height as u32);
    assert!(
        image
            .bytes()
            .as_chunks::<4>()
            .0
            .iter()
            .all(|v| i32::from_le_bytes(*v) == 65535)
    );
    assert_eq!(budget.used_bytes(), height * 4);
    drop(image);
    assert_eq!(budget.used_bytes(), 0);
}

#[tokio::test]
async fn oversized_tokens_and_envelopes_are_rejected_before_unbounded_decoder_work() {
    let body = format!(
        "{{\"Value\":[[{}]],\"Type\":2,\"Rank\":2}}",
        "1".repeat(129)
    );
    rejects(body.as_bytes()).await;
    let body = format!(
        "{{\"Value\":[[1]],\"Type\":2,\"Rank\":2,\"extra\":\"{}\"}}",
        "x".repeat(IMAGE_CHUNK_BYTES + 1)
    );
    rejects(body.as_bytes()).await;
    let mut value = serde_json::Map::new();
    for i in 0..33 {
        value.insert(format!("extra{i}"), json!(1));
    }
    value.insert("Value".into(), json!([[1]]));
    value.insert("Type".into(), json!(2));
    value.insert("Rank".into(), json!(2));
    rejects(&serde_json::to_vec(&value).unwrap()).await;
}

#[tokio::test]
async fn cancelled_partial_json_releases_staging_and_leaves_no_decoder_or_image() {
    let budget = ImageBudget::new(IMAGE_CHUNK_BYTES * 3).unwrap();
    let (mut output, mut input) = tokio::io::duplex(4096);
    let chunk = vec![b' '; IMAGE_CHUNK_BYTES];
    let sender = tokio::spawn(async move {
        output.write_all(&chunk).await.unwrap();
        std::future::pending::<()>().await;
    });
    let task_budget = budget.clone();
    let task = tokio::spawn(async move { read_json_image(&mut input, &task_budget, 17).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while budget.used_bytes() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(matches!(task.await,Err(error) if error.is_cancelled()));
    assert_eq!(budget.used_bytes(), 0);
    sender.abort();
    let _ = sender.await;
}

#[tokio::test]
async fn decoder_admission_bounds_waiting_streams_and_cancellation_frees_a_slot() {
    struct Waiting {
        id: usize,
        entered: std::sync::Arc<std::sync::Mutex<Vec<usize>>>,
        recorded: bool,
    }
    impl tokio::io::AsyncRead for Waiting {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            if !self.recorded {
                self.entered.lock().unwrap().push(self.id);
                self.recorded = true;
            }
            std::task::Poll::Pending
        }
    }
    let entered = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut tasks = Vec::new();
    for id in 0..5 {
        let mut reader = Waiting {
            id,
            entered: entered.clone(),
            recorded: false,
        };
        tasks.push(Some(tokio::spawn(async move {
            read_json_image(&mut reader, &ImageBudget::new(1).unwrap(), 17).await
        })));
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while entered.lock().unwrap().len() < 4 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(entered.lock().unwrap().len(), 4);
    let id = entered.lock().unwrap()[0];
    let first = tasks[id].take().unwrap();
    first.abort();
    assert!(matches!(first.await,Err(error) if error.is_cancelled()));
    tokio::time::timeout(Duration::from_secs(5), async {
        while entered.lock().unwrap().len() < 5 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    for task in tasks.into_iter().flatten() {
        task.abort();
        assert!(matches!(task.await,Err(error) if error.is_cancelled()));
    }
}
