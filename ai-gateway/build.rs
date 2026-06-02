fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut includes = vec!["src/proto"];
    for path in [
        "/usr/include",
        "/usr/local/include",
        "/usr/share/protobuf",
        "/usr/share/protobuf-compiler",
        "/usr/share/clickhouse/protos",
    ] {
        if std::path::Path::new(path)
            .join("google/protobuf/timestamp.proto")
            .exists()
        {
            includes.push(path);
            break;
        }
    }

    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        .extern_path(
            ".google.protobuf.Timestamp",
            "crate::google::protobuf::Timestamp",
        )
        .compile_protos(
            &["src/proto/evaluate.proto", "src/proto/payment.proto"],
            &includes,
        )?;
    Ok(())
}
