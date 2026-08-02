const child = Bun.spawn(
  [
    "cargo",
    "test",
    "--manifest-path",
    "painter/Cargo.toml",
    "canonical_protocol_fixture_is_current",
    "--",
    "--nocapture",
  ],
  {
    cwd: import.meta.dir.replace(/[/\\]scripts$/, ""),
    env: {
      ...Bun.env,
      UPDATE_PROTOCOL_FIXTURES: "1",
    },
    stdin: "inherit",
    stdout: "inherit",
    stderr: "inherit",
  },
);

process.exitCode = await child.exited;
