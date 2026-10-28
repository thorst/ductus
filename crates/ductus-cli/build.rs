// On Windows: compile the Event Log's messages (src/service/messages.rs) into ductus.exe, so Event
// Viewer shows our text for the "Ductus" source instead of "the description cannot be found".

#[allow(dead_code)]
mod messages {
    include!("src/service/messages.rs");
}

fn main() {
    println!("cargo:rerun-if-changed=src/service/messages.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let bin = out.join("messages.bin");
    std::fs::write(&bin, messages::message_table(messages::MESSAGES)).unwrap();
    // 1 = the table's resource ID; rc.exe takes the binary as is (a C string: backslashes doubled).
    let rc = out.join("messages.rc");
    let path = bin.display().to_string().replace('\\', "\\\\");
    std::fs::write(&rc, format!("1 MESSAGETABLE \"{path}\"\n")).unwrap();
    embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().unwrap();
}
