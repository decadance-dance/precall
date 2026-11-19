//! `plugin::load` reads the user config from the environment, so this binary holds one test
//! and sets the variables itself.

#[test]
fn a_bad_user_config_aborts_the_load_with_exit_4() {
    let root = std::env::temp_dir().join(format!("rrecall-load-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (config, store) = (root.join("config"), root.join("store"));
    std::fs::create_dir_all(config.join("rrecall")).unwrap();
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("rrecall.toml"), "").unwrap();
    std::fs::write(
        config.join("rrecall/config.toml"),
        "[[trusted]]\npath = \"/example\"\n",
    )
    .unwrap();
    // SAFETY: the only test in this binary, so no other thread reads the environment.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", &config) };
    let e = rrecall::plugin::load(&store).unwrap_err();
    assert_eq!(rrecall::error::exit_code(&e), 4, "{e:#}");
    assert!(format!("{e:#}").contains("hash"), "{e:#}");
    std::fs::write(config.join("rrecall/config.toml"), "").unwrap();
    assert!(rrecall::plugin::load(&store).unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&root);
}
