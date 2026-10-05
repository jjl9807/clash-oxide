fn main() {
    // rust-i18n reads these resources in a proc macro; Cargo must also track
    // them when only translations change between incremental builds.
    println!("cargo:rerun-if-changed=locales");
}
