use std::path::PathBuf;

#[derive(Clone, Copy)]
struct ProviderExpectation {
    job: &'static str,
    install_step: &'static str,
    prefix: &'static str,
    binary: &'static str,
    rejects_go_latest: bool,
}

fn job_block<'a>(workflow: &'a str, job: &str) -> Result<&'a str, String> {
    let marker = format!("  {job}:\n");
    let start = workflow
        .find(&marker)
        .ok_or_else(|| format!("missing workflow job {job}"))?;
    let mut offset = start + marker.len();
    for line in workflow[offset..].split_inclusive('\n') {
        if line.starts_with("  ") && !line.starts_with("    ") && line.trim_end().ends_with(':') {
            return Ok(&workflow[start..offset]);
        }
        offset += line.len();
    }
    Ok(&workflow[start..])
}

fn step_block<'a>(job: &'a str, step_name: &str) -> Result<&'a str, String> {
    let marker = format!("      - name: {step_name}\n");
    let start = job
        .find(&marker)
        .ok_or_else(|| format!("missing install step {step_name}"))?;
    let after_start = start + marker.len();
    let end = job[after_start..]
        .find("\n      - name: ")
        .map(|offset| after_start + offset)
        .unwrap_or(job.len());
    Ok(&job[start..end])
}

fn assignment<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    block
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(&format!("{name}=")))
}

fn provider_contract_error(job: &str, provider: ProviderExpectation) -> Result<(), String> {
    let install = step_block(job, provider.install_step)?;

    if install.contains("releases/latest/download") {
        return Err(format!(
            "{} install step uses the historical floating release endpoint",
            provider.binary
        ));
    }
    if provider.rejects_go_latest && install.contains("@latest") {
        return Err(format!(
            "{} install step uses the historical floating Go acquisition",
            provider.binary
        ));
    }

    let version_name = format!("{}_VERSION", provider.prefix);
    let version = assignment(install, &version_name)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{} version pin missing", provider.prefix))?;
    if version.contains("latest") {
        return Err(format!(
            "{} version pin must not float to latest",
            provider.prefix
        ));
    }

    let sha_name = format!("{}_SHA256", provider.prefix);
    let sha = assignment(install, &sha_name)
        .ok_or_else(|| format!("{} checksum pin missing", provider.prefix))?;
    if sha.len() != 64 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "{} checksum must be exactly 64 hexadecimal characters",
            provider.prefix
        ));
    }

    let version_reference = format!("${{{}_VERSION}}", provider.prefix);
    let mut command_lines = install
        .lines()
        .skip_while(|line| !line.trim_start().starts_with("curl "));
    let first_download_line = command_lines
        .next()
        .ok_or_else(|| format!("{} download command missing", provider.binary))?;
    let mut download = first_download_line.trim_start().to_owned();
    let mut continued = first_download_line.trim_end().ends_with('\\');
    while continued {
        let line = command_lines.next().ok_or_else(|| {
            format!(
                "{} download command ends in a dangling continuation",
                provider.binary
            )
        })?;
        download.push('\n');
        download.push_str(line.trim_start());
        continued = line.trim_end().ends_with('\\');
    }
    if !download.contains(&version_reference) {
        return Err(format!(
            "{} download command must interpolate its pinned VERSION variable",
            provider.binary
        ));
    }
    let sha_reference = format!("${{{}_SHA256}}", provider.prefix);
    let checksum = install
        .lines()
        .find(|line| {
            let command = line.trim_start();
            command.starts_with("echo ") && command.contains("sha256sum --check --status -")
        })
        .ok_or_else(|| format!("{} download is not checksum-verified", provider.binary))?;
    if !checksum.contains(&sha_reference) {
        return Err(format!(
            "{} checksum command does not verify the pinned SHA256",
            provider.binary
        ));
    }

    let probe = format!("run: {} --version", provider.binary);
    let install_offset = job
        .find(install)
        .expect("install block is a slice of its job block");
    let checksum_offset = install_offset
        + install
            .find(checksum)
            .expect("checksum command is a slice of its install block");
    let probe_offset = job
        .find(&probe)
        .ok_or_else(|| format!("{} version probe missing", provider.binary))?;
    if checksum_offset >= probe_offset {
        return Err(format!(
            "{} version probe must run after checksum verification",
            provider.binary
        ));
    }

    Ok(())
}

#[test]
fn historical_floating_provider_forms_are_rejected_per_provider_block() {
    let ruby = "      - name: Install scip-ruby\n        run: |\n          https://github.com/sourcegraph/scip-ruby/releases/latest/download/scip-ruby\n";
    let clang = "      - name: Install scip-clang\n        run: |\n          https://github.com/sourcegraph/scip-clang/releases/latest/download/scip-clang\n";
    let go = "      - name: Install scip-go\n        run: |\n          go install github.com/scip-code/scip-go/cmd/scip-go@latest\n";

    let ruby_error = provider_contract_error(
        ruby,
        ProviderExpectation {
            job: "scip-ruby",
            install_step: "Install scip-ruby",
            prefix: "SCIP_RUBY",
            binary: "scip-ruby",
            rejects_go_latest: false,
        },
    )
    .expect_err("Ruby historical endpoint must be rejected");
    assert!(ruby_error.contains("historical floating release endpoint"));

    let clang_error = provider_contract_error(
        clang,
        ProviderExpectation {
            job: "scip-clang",
            install_step: "Install scip-clang",
            prefix: "SCIP_CLANG",
            binary: "scip-clang",
            rejects_go_latest: false,
        },
    )
    .expect_err("Clang historical endpoint must be rejected");
    assert!(clang_error.contains("historical floating release endpoint"));

    let go_error = provider_contract_error(
        go,
        ProviderExpectation {
            job: "scip-go",
            install_step: "Install scip-go",
            prefix: "SCIP_GO",
            binary: "scip-go",
            rejects_go_latest: true,
        },
    )
    .expect_err("Go historical acquisition must be rejected");
    assert!(go_error.contains("historical floating Go acquisition"));
}

#[test]
fn provider_contract_rejects_version_and_checksum_variables_only_mentioned_in_comments() {
    let go = r#"  scip-go:
    steps:
      - name: Install scip-go
        run: |
          SCIP_GO_VERSION=v0.2.7
          SCIP_GO_SHA256=5bfe39016ca04f5b3b1cce41d1b63ea120a7d7e93b55407bfb17a6b02d18135a
          # curl https://example.invalid/${SCIP_GO_VERSION}/scip-go
          curl https://example.invalid/v0.2.7/scip-go
          # echo "${SCIP_GO_SHA256}  scip-go" | sha256sum --check --status -
          echo "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  scip-go" | sha256sum --check --status -
      - name: Record pinned scip-go version
        run: scip-go --version
"#;

    let error = provider_contract_error(
        go,
        ProviderExpectation {
            job: "scip-go",
            install_step: "Install scip-go",
            prefix: "SCIP_GO",
            binary: "scip-go",
            rejects_go_latest: true,
        },
    )
    .expect_err("comments must not satisfy acquisition-pin assertions");
    assert!(error.contains("download command must interpolate"));
}

#[test]
fn nightly_release_providers_are_pinned_verified_and_version_probed() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let workflow = std::fs::read_to_string(root.join(".github/workflows/scip-nightly.yml"))
        .expect("checked-in nightly workflow");

    for provider in [
        ProviderExpectation {
            job: "scip-go",
            install_step: "Install scip-go",
            prefix: "SCIP_GO",
            binary: "scip-go",
            rejects_go_latest: true,
        },
        ProviderExpectation {
            job: "scip-ruby",
            install_step: "Install scip-ruby",
            prefix: "SCIP_RUBY",
            binary: "scip-ruby",
            rejects_go_latest: false,
        },
        ProviderExpectation {
            job: "scip-clang",
            install_step: "Install scip-clang",
            prefix: "SCIP_CLANG",
            binary: "scip-clang",
            rejects_go_latest: false,
        },
    ] {
        let job = job_block(&workflow, provider.job)
            .unwrap_or_else(|error| panic!("{}: {error}", provider.job));
        provider_contract_error(job, provider)
            .unwrap_or_else(|error| panic!("{}: {error}", provider.job));
    }
}
