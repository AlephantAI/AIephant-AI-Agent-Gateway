use crate::validation::ValidationPolicy;
use base64::{Engine, engine::general_purpose::STANDARD};
use http::{HeaderMap, StatusCode};
use image::{DynamicImage, ImageFormat, RgbImage};
use serde_json::{Value, json};
use std::io::Cursor;

fn image_bytes(format: ImageFormat, width: u32, height: u32) -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(RgbImage::new(width, height))
        .write_to(&mut output, format)
        .unwrap();
    output.into_inner()
}
fn image_rule() -> Value {
    json!({"kind":"image","select":{"pointer":"/assets","each":"object_value","required":false},
        "data_pointer":"/data_base64","format_pointer":"/format","encoding":"base64",
        "formats":["png","jpeg"],"max_decoded_bytes":16777216,
        "width":{"min":280,"max":1568},"height":{"min":280,"max":1568},
        "forbidden_fields":["/part"],"required_header":{"name":"Images-Enabled","value":"true"},
        "references":{"search":[{"pointer":"/state","required":false}],"max_key_bytes":1024,"max_total_key_bytes":65536}})
}
fn box_rule() -> Value {
    json!({"kind":"bounding_box","select":{"pointer":"/answers","each":"object_value","where":{"pointer":"/type","equals":"box"}},
        "fields":{"left":"/box/left","top":"/box/top","right":"/box/right","bottom":"/box/bottom"},"min":0,"max":1,
        "confidence":{"pointer":"/confidence","nullable":true},
        "request_selector":{"pointer":"/questions","each":"object_value","where":{"pointer":"/type","equals":"box"}},
        "requires_image":true,"required_header":{"name":"Images-Enabled","value":"true"}})
}
fn policy() -> ValidationPolicy {
    ValidationPolicy::parse(
        &json!({"version":1,"request_checks":[image_rule()],"response_checks":[box_rule()]}),
    )
    .unwrap()
}
fn headers() -> HeaderMap {
    [("images-enabled".parse().unwrap(), "true".parse().unwrap())]
        .into_iter()
        .collect()
}
fn request(data: &[u8], format: &str) -> Value {
    json!({"state":{"nested":["asset/key~1"]},"assets":{"asset/key~1":{"format":format,"data_base64":STANDARD.encode(data)}},
        "questions":{"locate/key~1":{"type":"box"},"text":{"type":"choice"}}})
}
fn answer() -> Value {
    json!({"answers":{"locate/key~1":{"type":"box","box":{"left":0,"top":0.1,"right":1,"bottom":0.9},"confidence":null},
        "text":{"type":"choice","choice":"a"}},"usage":{"input_tokens":120}})
}
fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

#[test]
fn real_png_jpeg_and_mixed_answers_preserve_null_confidence() {
    for (format, declared) in [
        (ImageFormat::Png, "png"),
        (ImageFormat::Jpeg, "jpg"),
        (ImageFormat::Jpeg, "jpeg"),
    ] {
        let original = request(&image_bytes(format, 280, 280), declared);
        let context = policy()
            .validate_request(&headers(), &bytes(&original))
            .unwrap();
        let mut response = answer();
        for confidence in [Value::Null, json!(0), json!(1)] {
            response["answers"]["locate/key~1"]["confidence"] = confidence;
            policy()
                .validate_response(&context, StatusCode::OK, &bytes(&response))
                .unwrap();
        }
        assert!(original["assets"]["asset/key~1"]["data_base64"].is_string());
    }
}

#[test]
fn corrupt_base64_images_and_asset_contracts_are_rejected() {
    let valid = request(&image_bytes(ImageFormat::Png, 280, 280), "png");
    for encoded in [
        "AA",
        "AA==\n",
        "data:image/png;base64,AA==",
        "AB==",
        "_w==",
        "AAAA====",
    ] {
        let mut body = valid.clone();
        body["assets"]["asset/key~1"]["data_base64"] = json!(encoded);
        assert!(
            policy()
                .validate_request(&headers(), &bytes(&body))
                .is_err()
        );
    }
    for (data, declared) in [
        (vec![0; 100], "png"),
        (image_bytes(ImageFormat::Png, 279, 280), "png"),
        (image_bytes(ImageFormat::Png, 1569, 280), "png"),
        (image_bytes(ImageFormat::Png, 280, 280), "jpeg"),
    ] {
        assert!(
            policy()
                .validate_request(&headers(), &bytes(&request(&data, declared)))
                .is_err()
        );
    }
    let png = image_bytes(ImageFormat::Png, 280, 280);
    assert!(
        policy()
            .validate_request(&headers(), &bytes(&request(&png[..png.len() / 2], "png")))
            .is_err()
    );
    assert!(
        policy()
            .validate_request(&headers(), &bytes(&request(&png[..png.len() - 12], "png")))
            .is_err(),
        "PNG without IEND is incomplete"
    );
    let jpeg = image_bytes(ImageFormat::Jpeg, 280, 280);
    assert!(
        policy()
            .validate_request(
                &headers(),
                &bytes(&request(&jpeg[..jpeg.len() - 2], "jpeg"))
            )
            .is_err()
    );
    let mut body = valid.clone();
    body["assets"]["asset/key~1"]["part"] = Value::Null;
    assert!(
        policy()
            .validate_request(&headers(), &bytes(&body))
            .is_err()
    );
    let mut body = valid.clone();
    body["state"] = json!({"asset/key~1":"key only"});
    assert!(
        policy()
            .validate_request(&headers(), &bytes(&body))
            .is_err(),
        "object keys are not image references"
    );
    let mut rule = image_rule();
    rule["max_decoded_bytes"] = json!(png.len() - 1);
    let small = ValidationPolicy::parse(&json!({"version":1,"request_checks":[rule]})).unwrap();
    assert!(small.validate_request(&headers(), &bytes(&valid)).is_err());
}

#[test]
fn animated_png_requires_explicit_permission_and_complete_frames() {
    let mut data = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut data, 280, 280);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_animated(1, 0).unwrap();
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&vec![0; 280 * 280 * 3]).unwrap();
        writer.finish().unwrap();
    }
    let body = request(&data, "png");
    assert!(
        policy()
            .validate_request(&headers(), &bytes(&body))
            .is_err()
    );
    let mut rule = image_rule();
    rule["allow_animation"] = json!(true);
    let animated = ValidationPolicy::parse(&json!({"version":1,"request_checks":[rule]})).unwrap();
    animated
        .validate_request(&headers(), &bytes(&body))
        .unwrap();
    assert!(
        animated
            .validate_request(
                &headers(),
                &bytes(&request(&data[..data.len() - 12], "png"))
            )
            .is_err()
    );
}

#[test]
fn image_headers_and_request_response_correspondence_cannot_be_bypassed() {
    let body = request(&image_bytes(ImageFormat::Png, 280, 280), "png");
    assert!(
        policy()
            .validate_request(&HeaderMap::new(), &bytes(&body))
            .is_err()
    );
    let mut duplicate = headers();
    duplicate.append("images-enabled", "true".parse().unwrap());
    assert!(
        policy()
            .validate_request(&duplicate, &bytes(&body))
            .is_err()
    );
    let mut no_image = body.clone();
    no_image["assets"] = json!({});
    assert!(
        policy()
            .validate_request(&headers(), &bytes(&no_image))
            .is_err()
    );
    let mut malformed = body.clone();
    malformed["assets"] = Value::Null;
    assert!(
        policy()
            .validate_request(&headers(), &bytes(&malformed))
            .is_err()
    );
    let context = policy()
        .validate_request(&headers(), &bytes(&body))
        .unwrap();
    let mut response = answer();
    response["answers"]
        .as_object_mut()
        .unwrap()
        .remove("locate/key~1");
    assert!(
        policy()
            .validate_response(&context, StatusCode::OK, &bytes(&response))
            .is_err()
    );
    response = answer();
    response["answers"]["locate/key~1"]["type"] = json!("noul");
    assert!(
        policy()
            .validate_response(&context, StatusCode::OK, &bytes(&response))
            .is_err()
    );
    policy()
        .validate_response(&context, StatusCode::BAD_REQUEST, b"upstream error")
        .unwrap();
    policy()
        .validate_request(
            &HeaderMap::new(),
            br#"{"questions":{"text":{"type":"noul"}}}"#,
        )
        .unwrap();
}

#[test]
fn box_missing_null_reversed_and_out_of_range_values_are_distinct() {
    let context = policy()
        .validate_request(
            &headers(),
            &bytes(&request(&image_bytes(ImageFormat::Png, 280, 280), "png")),
        )
        .unwrap();
    for (pointer, value) in [
        ("/box/left", json!(-0.1)),
        ("/box/top", json!(1)),
        ("/box/right", Value::Null),
        ("/box/bottom", json!(0.1)),
        ("/confidence", json!(-0.1)),
        ("/confidence", json!(1.1)),
        ("/confidence", json!("0.5")),
    ] {
        let mut response = answer();
        *response["answers"]["locate/key~1"]
            .pointer_mut(pointer)
            .unwrap() = value;
        assert!(
            policy()
                .validate_response(&context, StatusCode::OK, &bytes(&response))
                .is_err()
        );
    }
    let mut response = answer();
    response["answers"]["locate/key~1"]
        .as_object_mut()
        .unwrap()
        .remove("confidence");
    assert!(
        policy()
            .validate_response(&context, StatusCode::OK, &bytes(&response))
            .is_err()
    );
}

#[test]
fn a_second_api_uses_arrays_different_fields_and_pixel_boxes_without_provider_code() {
    let policy = ValidationPolicy::parse(&json!({"version":1,
        "request_checks":[{"kind":"image","select":{"pointer":"/pictures","each":"array_item","min_items":1},
            "data_pointer":"/content","encoding":"base64","formats":["png"],"max_decoded_bytes":10000,
            "width":{"min":1,"max":1000},"height":{"min":1,"max":1000}}],
        "response_checks":[{"kind":"bounding_box","select":{"pointer":"/detections","each":"array_item","min_items":1},
            "fields":{"left":"/x","top":"/y","right":"/w","bottom":"/h"},"min":0,"max":1000,
            "representation":"origin_size","confidence":{"pointer":"/p","required":false}}]})).unwrap();
    let request =
        json!({"pictures":[{"content":STANDARD.encode(image_bytes(ImageFormat::Png,10,20))}]});
    let context = policy
        .validate_request(&HeaderMap::new(), &bytes(&request))
        .unwrap();
    policy
        .validate_response(
            &context,
            StatusCode::OK,
            br#"{"detections":[{"x":100,"y":200,"w":50,"h":60}]}"#,
        )
        .unwrap();
    assert!(
        policy
            .validate_response(
                &context,
                StatusCode::OK,
                br#"{"detections":[{"x":990,"y":200,"w":50,"h":60}]}"#
            )
            .is_err()
    );
    assert!(
        policy
            .validate_response(&context, StatusCode::OK, br#"{"detections":[]}"#)
            .is_err()
    );
    assert!(
        policy
            .validate_request(&HeaderMap::new(), br#"{"pictures":null}"#)
            .is_err()
    );
}

#[test]
fn malformed_rules_and_duplicate_json_keys_fail_with_value_free_errors() {
    let mut rule = image_rule();
    rule["select"]["pointer"] = json!("/bad~escape");
    assert!(ValidationPolicy::parse(&json!({"version":1,"request_checks":[rule]})).is_err());
    let mut invalid = request(&image_bytes(ImageFormat::Png, 280, 280), "png");
    invalid["assets"]["asset/key~1"]["data_base64"] = json!("private-payload-value");
    let error = policy()
        .validate_request(&headers(), &bytes(&invalid))
        .err()
        .unwrap()
        .to_string();
    assert!(!error.contains("private-payload-value") && !error.contains("asset/key~1"));
    assert!(
        policy()
            .validate_request(&headers(), br#"{"assets":{},"assets":{"x":"y"}}"#)
            .is_err()
    );
}

#[test]
fn many_small_compressed_images_share_one_decoding_work_budget() {
    let encoded = STANDARD.encode(image_bytes(ImageFormat::Png, 1024, 1024));
    let policy = ValidationPolicy::parse(&json!({"version":1,"request_checks":[{
        "kind":"image","select":{"pointer":"/pictures","each":"array_item"},
        "data_pointer":"/data","encoding":"base64","formats":["png"],"max_decoded_bytes":1000000,
        "width":{"min":1,"max":1024},"height":{"min":1,"max":1024}}]}))
    .unwrap();
    let body = json!({"pictures":vec![json!({"data":encoded});22]});
    assert!(
        policy
            .validate_request(&HeaderMap::new(), &bytes(&body))
            .is_err(),
        "compressed uploads must not allow unbounded total decode work"
    );
}

#[test]
fn confidence_range_is_configurable_for_percent_based_apis() {
    let config = json!({"version":1,"response_checks":[{
        "kind":"bounding_box","select":{"pointer":"/box"},
        "fields":{"left":"/x1","top":"/y1","right":"/x2","bottom":"/y2"},"min":0,"max":1000,
        "confidence":{"pointer":"/certainty","min":0,"max":100}}]});
    let policy =
        ValidationPolicy::parse(&config).expect("confidence range must not be provider-specific");
    let context = policy.validate_request(&HeaderMap::new(), b"{}").unwrap();
    policy
        .validate_response(
            &context,
            StatusCode::OK,
            br#"{"box":{"x1":1,"y1":2,"x2":3,"y2":4,"certainty":90}}"#,
        )
        .unwrap();
    assert!(
        policy
            .validate_response(
                &context,
                StatusCode::OK,
                br#"{"box":{"x1":1,"y1":2,"x2":3,"y2":4,"certainty":101}}"#
            )
            .is_err()
    );
}
