{
  "targets": [
    {
      "target_name": "cavalry_addon",
      "sources": ["src/addon.c"],
      "include_dirs": ["../../crates/cavalry-ffi/include"],
      "libraries": ["<(module_root_dir)/../../target/release/libcavalry_ffi.a"],
      "cflags": ["-std=c11", "-Wall", "-Wextra"],
      "conditions": [
        ["OS=='mac'", {
          "xcode_settings": {"OTHER_CFLAGS": ["-std=c11", "-Wall", "-Wextra"]},
          "link_settings": {"libraries": ["-framework CoreFoundation", "-framework Security", "-lresolv"]}
        }],
        ["OS=='linux'", {
          "link_settings": {"libraries": ["-lpthread", "-ldl", "-lm"]}
        }]
      ]
    }
  ]
}
