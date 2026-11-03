use hfc_runtime::jimage::JImage;

#[test]
fn open_jimage() {
    let path = "/home/daniel/.local/share/horizon-launcher/minecraft/runtime/java-runtime-epsilon/linux/java-runtime-epsilon/lib/modules";
    eprintln!("opening");
    let image = JImage::open(path).unwrap();
    eprintln!("opened, locations={}", image.location_size());
    for name in [
        "java/lang/Object",
        "java/lang/Class",
        "java/util/AbstractMap",
    ] {
        eprintln!("contains {name}: {}", image.contains_class(name));
        match image.class_bytes(name) {
            Ok(bytes) => {
                eprintln!("  {} bytes", bytes.len());
                match hfc_runtime::class::Class::from_bytes(&bytes) {
                    Ok(class) => eprintln!("  parsed: {}", class.this_class),
                    Err(e) => eprintln!("  parse error: {e}"),
                }
            }
            Err(e) => eprintln!("  read error: {e}"),
        }
    }
}
