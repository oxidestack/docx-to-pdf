//! The installation guide, `docs/guide/installation.md`, against the files it shows, the chart and the code.
//!
//! Without Docker:
//!
//! - **The compose snippets are the shipped files.** Every file under `examples/compose/` is, byte for byte, a fenced
//!   block of the page, and the page names its path. Each compose file runs the image of this version, publishes the
//!   service's default port on `127.0.0.1` only, sets only variables the service reads (rows of
//!   `docs/guide/configuration.md`), and limits memory to the page's formula for its pool size. The telemetry one
//!   exports to its collector service on a port the collector's configuration receives OTLP over HTTP on.
//! - **The `docker run` lines** run the image of this version, first on every interface, then bound to `127.0.0.1`.
//! - **The `helm` commands are the install `tools/chart/kind-test.sh` tests**: the same release name and namespace, the
//!   chart published under its `Chart.yaml` name and version, and no option besides the version, namespace, values file
//!   and `--skip-tests`. The page's `values.yaml` sets only values the chart has (`charts/docx-to-pdf/values.yaml`), and
//!   its `config` only variables the service reads.
//! - **The NetworkPolicy is the chart's** (`networkPolicy`, on by default), not one written by hand on the page: the page
//!   names only values the chart has, its values hold the collector rule to the namespace of the collector they name,
//!   and the template admits ingress to 8080 and egress to DNS and the OTLP endpoint's port.
//! - Every relative link resolves.
//!
//! With Docker and helm (`#[ignore]`d; `DOCX_TO_PDF_IMAGE=docx-to-pdf:dev cargo test --test installation -- --ignored`,
//! `docx-to-pdf:dev` from `make image`, the default):
//!
//! - `docker compose up` of each compose file, its image replaced by the local one and its port by a free one,
//!   converts a document to a PDF; with telemetry, the collector prints the traces it received.
//! - `helm template` of the in-tree chart with the page's `values.yaml` renders a NetworkPolicy that selects the pods
//!   and allows ingress to 8080 and egress to DNS and the collector's namespace and port only, the collector endpoint,
//!   and the memory of the page's formula.
//!
//! No LibreOffice needed outside the image.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use docx_to_pdf::config::{DEFAULT_INSTANCE_HARD_MEMORY, DEFAULT_PORT};

const PAGE: &str = "docs/guide/installation.md";
const CONFIGURATION: &str = "docs/guide/configuration.md";
const COMPOSE_DIR: &str = "examples/compose";
const CHART: &str = "charts/docx-to-pdf";
const KIND_TEST: &str = "tools/chart/kind-test.sh";
const IMAGE: &str = "quay.io/oxidestack/docx-to-pdf";
const CHART_REPOSITORY: &str = "oci://quay.io/oxidestack/charts";
const MIB: u64 = 1024 * 1024;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &str) -> String {
    std::fs::read_to_string(root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn page() -> String {
    std::fs::read_to_string(root().join(PAGE))
        .unwrap_or_else(|e| panic!("{PAGE}: {e}; the installation guide is missing"))
}

// --- reading Markdown ----------------------------------------------------------------------------------------------

/// Every fenced block: (info string, text with a final newline per line).
fn blocks(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut open: Option<(String, String)> = None;
    for line in text.lines() {
        match (&mut open, line.strip_prefix("```")) {
            (None, Some(info)) => open = Some((info.trim().to_owned(), String::new())),
            (Some(_), Some(_)) => out.push(open.take().unwrap()),
            (Some((_, body)), None) => {
                body.push_str(line);
                body.push('\n');
            }
            (None, None) => {}
        }
    }
    assert!(open.is_none(), "{PAGE}: a fence is never closed");
    out
}

/// The text of the section under `## <heading>`, up to the next `## `.
fn section<'a>(page: &'a str, heading: &str) -> &'a str {
    let start = page
        .find(&format!("\n## {heading}\n"))
        .unwrap_or_else(|| panic!("{PAGE}: no section {heading:?}"))
        + 1;
    let end = page[start + 3..]
        .find("\n## ")
        .map_or(page.len(), |i| start + 3 + i);
    &page[start..end]
}

/// The commands of the `sh` blocks of `text`, continuation lines joined, as words (no quoting is used).
fn commands(text: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for (info, body) in blocks(text) {
        if info != "sh" {
            continue;
        }
        let mut current = String::new();
        for line in body.lines() {
            if let Some(head) = line.trim_end().strip_suffix('\\') {
                current.push_str(head);
                current.push(' ');
            } else {
                current.push_str(line);
                let words: Vec<String> = current.split_whitespace().map(str::to_owned).collect();
                if !words.is_empty() && !words[0].starts_with('#') {
                    out.push(words);
                }
                current.clear();
            }
        }
    }
    out
}

/// The value after `flag` in `words`, if the flag is there.
fn after<'a>(words: &'a [String], flag: &str) -> Option<&'a str> {
    let i = words.iter().position(|w| w == flag)?;
    Some(
        words
            .get(i + 1)
            .unwrap_or_else(|| panic!("{flag} takes a value"))
            .as_str(),
    )
}

/// Every Markdown link of the text: its target.
fn link_targets(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (at, _) in text.match_indices("](") {
        let target = &text[at + 2..];
        if let Some(end) = target.find(')') {
            out.push(target[..end].to_owned());
        }
    }
    out
}

/// The variables the configuration reference has a row for, `<SIGNAL>` expanded.
fn variables() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in read(CONFIGURATION).lines() {
        let Some(name) = line
            .strip_prefix("| `")
            .and_then(|rest| rest.split_once('`'))
            .map(|(name, _)| name)
        else {
            continue;
        };
        if name.contains("<SIGNAL>") {
            for signal in ["TRACES", "LOGS", "METRICS"] {
                out.insert(name.replace("<SIGNAL>", signal));
            }
        } else {
            out.insert(name.to_owned());
        }
    }
    assert!(
        out.contains("DOCX_TO_PDF_POOL_SIZE"),
        "{CONFIGURATION}: no rows read"
    );
    out
}

// --- reading YAML, the plain subset these files use ----------------------------------------------------------------

/// Every `key: value` of a YAML text as (dotted path, value), by indentation; comments and list items skipped.
fn yaml_paths(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack: Vec<(usize, String)> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
            continue;
        }
        let indent = line.len() - trimmed.len();
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        if key.contains(' ') && !key.starts_with('"') {
            continue;
        }
        while stack.last().is_some_and(|(i, _)| *i >= indent) {
            stack.pop();
        }
        let key = key.trim_matches('"').to_owned();
        let path = stack
            .iter()
            .map(|(_, k)| k.as_str())
            .chain([key.as_str()])
            .collect::<Vec<_>>()
            .join(".");
        let value = value.split(" #").next().unwrap().trim().to_owned();
        out.push((path, value));
        stack.push((indent, key));
    }
    out
}

fn yaml_value(text: &str, path: &str) -> Option<String> {
    yaml_paths(text)
        .into_iter()
        .find(|(p, _)| p == path)
        .map(|(_, v)| v.trim_matches('"').to_owned())
}

/// The list items directly under `path` (`- item` lines, or a flow list `[a, b]`).
fn yaml_list(text: &str, path: &str) -> Vec<String> {
    if let Some(flow) = yaml_value(text, path).filter(|v| v.starts_with('[')) {
        return flow
            .trim_matches(['[', ']'])
            .split(',')
            .map(|s| s.trim().trim_matches('"').to_owned())
            .filter(|s| !s.is_empty())
            .collect();
    }
    let mut out = Vec::new();
    let mut inside = false;
    let mut depth = 0;
    let last = path.rsplit('.').next().unwrap();
    for (n, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if inside {
            if let Some(item) = trimmed.strip_prefix("- ") {
                out.push(item.trim().trim_matches('"').to_owned());
                continue;
            }
            if !trimmed.is_empty() && indent <= depth {
                break;
            }
        } else if trimmed == format!("{last}:")
            && yaml_paths(&text.lines().take(n + 1).collect::<Vec<_>>().join("\n"))
                .last()
                .is_some_and(|(p, _)| p == path)
        {
            inside = true;
            depth = indent;
        }
    }
    out
}

// --- the compose files ---------------------------------------------------------------------------------------------

/// Every file under `examples/compose/`, relative to the repository.
fn compose_files() -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&root().join(COMPOSE_DIR), &mut files);
    let mut out: Vec<String> = files
        .iter()
        .map(|p| {
            p.strip_prefix(root())
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    out.sort();
    out
}

/// The compose files proper (those named `compose.yaml`).
fn compose_yamls() -> Vec<String> {
    let out: Vec<String> = compose_files()
        .into_iter()
        .filter(|f| f.ends_with("/compose.yaml"))
        .collect();
    assert!(
        out.contains(&format!("{COMPOSE_DIR}/compose.yaml")),
        "{COMPOSE_DIR}/compose.yaml: missing"
    );
    out
}

fn this_image() -> String {
    format!("{IMAGE}:{}", env!("CARGO_PKG_VERSION"))
}

#[test]
fn every_compose_file_is_a_block_of_the_page() {
    let page = page();
    let blocks: Vec<String> = blocks(&page).into_iter().map(|(_, b)| b).collect();
    for file in compose_files() {
        let text = read(&file);
        assert!(
            blocks.contains(&text),
            "{PAGE}: no fenced block is {file} as it is"
        );
        assert!(
            page.contains(&format!("`{file}`")),
            "{PAGE}: does not name {file}"
        );
    }
}

#[test]
fn every_compose_file_runs_this_version_on_localhost_with_settings_the_service_reads() {
    let variables = variables();
    for file in compose_yamls() {
        let text = read(&file);
        let service = "services.docx-to-pdf";
        assert_eq!(
            yaml_value(&text, &format!("{service}.image")).as_deref(),
            Some(this_image().as_str()),
            "{file}: runs the image of this version"
        );
        assert_eq!(
            yaml_list(&text, &format!("{service}.ports")),
            [format!("127.0.0.1:{DEFAULT_PORT}:{DEFAULT_PORT}")],
            "{file}: publishes the default port on 127.0.0.1 only"
        );

        let environment: Vec<(String, String)> = yaml_paths(&text)
            .into_iter()
            .filter_map(|(p, v)| {
                let name = p.strip_prefix(&format!("{service}.environment."))?;
                Some((name.to_owned(), v.trim_matches('"').to_owned()))
            })
            .collect();
        for (name, _) in &environment {
            assert!(
                variables.contains(name),
                "{file}: {name} is not a row of {CONFIGURATION}"
            );
        }

        let pool: u64 = environment
            .iter()
            .find(|(n, _)| n == "DOCX_TO_PDF_POOL_SIZE")
            .map_or(1, |(_, v)| v.parse().unwrap());
        assert!(
            !environment
                .iter()
                .any(|(n, _)| n == "DOCX_TO_PDF_INSTANCE_HARD_MEMORY"),
            "{file}: the memory check below assumes the default hard memory"
        );
        let memory = 256 + pool * (DEFAULT_INSTANCE_HARD_MEMORY / MIB + 256);
        assert_eq!(
            yaml_value(&text, &format!("{service}.mem_limit")),
            Some(format!("{memory}m")),
            "{file}: mem_limit is 256 MiB + pool size × (hard memory + 256 MiB)"
        );
        assert_eq!(
            yaml_value(&text, &format!("{service}.read_only")).as_deref(),
            Some("true"),
            "{file}: a read-only root"
        );
    }
}

#[test]
fn the_telemetry_compose_file_exports_to_its_collector() {
    let file = format!("{COMPOSE_DIR}/telemetry/compose.yaml");
    let text = read(&file);
    let endpoint = yaml_value(
        &text,
        "services.docx-to-pdf.environment.OTEL_EXPORTER_OTLP_ENDPOINT",
    )
    .unwrap_or_else(|| panic!("{file}: no OTEL_EXPORTER_OTLP_ENDPOINT"));
    let (host, port) = endpoint
        .strip_prefix("http://")
        .and_then(|h| h.split_once(':'))
        .unwrap_or_else(|| panic!("{endpoint}: http://host:port"));
    assert!(
        yaml_paths(&text)
            .iter()
            .any(|(p, _)| p == &format!("services.{host}")),
        "{file}: no service {host}"
    );
    let config = read(&format!("{COMPOSE_DIR}/telemetry/otel-collector.yaml"));
    assert_eq!(
        yaml_value(&config, "receivers.otlp.protocols.http.endpoint"),
        Some(format!("0.0.0.0:{port}")),
        "the collector receives OTLP over HTTP on {port}"
    );
    for signal in ["traces", "metrics", "logs"] {
        assert_eq!(
            yaml_list(&config, &format!("service.pipelines.{signal}.receivers")),
            ["otlp"],
            "the collector's {signal} pipeline"
        );
    }
}

// --- docker run ----------------------------------------------------------------------------------------------------

#[test]
fn the_docker_run_lines_publish_the_port_then_bind_it_to_localhost() {
    let page = page();
    let runs: Vec<Vec<String>> = commands(section(&page, "Docker"))
        .into_iter()
        .filter(|w| w.len() > 1 && w[0] == "docker" && w[1] == "run")
        .collect();
    let published: Vec<&str> = runs
        .iter()
        .map(|w| {
            assert_eq!(
                w.last().unwrap(),
                &this_image(),
                "{PAGE}: docker run of this version"
            );
            after(w, "-p").expect("docker run publishes a port")
        })
        .collect();
    assert_eq!(
        published,
        [
            format!("{DEFAULT_PORT}:{DEFAULT_PORT}"),
            format!("127.0.0.1:{DEFAULT_PORT}:{DEFAULT_PORT}")
        ],
        "{PAGE}: the port published, then bound to 127.0.0.1"
    );
}

// --- Kubernetes ----------------------------------------------------------------------------------------------------

/// `NAME=value` of a shell script.
fn shell_assignment(script: &str, name: &str) -> String {
    script
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{name}=")))
        .unwrap_or_else(|| panic!("{KIND_TEST}: no {name}="))
        .trim_matches('"')
        .to_owned()
}

/// The chart's `name` and `version`.
fn chart_name_and_version() -> (String, String) {
    let chart = read(&format!("{CHART}/Chart.yaml"));
    (
        yaml_value(&chart, "name").unwrap(),
        yaml_value(&chart, "version").unwrap(),
    )
}

/// The page's `values.yaml`: the yaml block whose first line is `# values.yaml`.
fn page_values(page: &str) -> String {
    blocks(section(page, "Kubernetes"))
        .into_iter()
        .find(|(info, b)| info == "yaml" && b.starts_with("# values.yaml"))
        .map_or_else(
            || panic!("{PAGE}: no yaml block starting `# values.yaml`"),
            |(_, b)| b,
        )
}

#[test]
fn the_helm_commands_are_the_install_the_kind_test_runs() {
    let page = page();
    let script = read(KIND_TEST);
    let release = shell_assignment(&script, "RELEASE");
    let namespace = shell_assignment(&script, "NS");
    assert!(
        script.contains(r#"helm install "$RELEASE" "$CHART""#)
            && script.contains(r#"--namespace "$NS" --create-namespace"#),
        "{KIND_TEST}: no longer installs $RELEASE into $NS"
    );
    let (name, version) = chart_name_and_version();

    let helm: Vec<Vec<String>> = commands(section(&page, "Kubernetes"))
        .into_iter()
        .filter(|w| w[0] == "helm")
        // Up to a redirection of its output.
        .map(|w| w.into_iter().take_while(|w| w != ">").collect())
        .collect();
    let installs = helm
        .iter()
        .filter(|w| w[1] == "install" || (w[1] == "upgrade" && w.contains(&"--install".to_owned())))
        .count();
    assert!(installs >= 1, "{PAGE}: no helm install");
    for words in &helm {
        assert!(
            ["install", "upgrade", "template"].contains(&words[1].as_str()),
            "{PAGE}: {words:?}"
        );
        let positional: Vec<&String> = {
            let mut out = Vec::new();
            let mut args = words[2..].iter();
            while let Some(w) = args.next() {
                match w.as_str() {
                    "--version" | "--namespace" | "--values" => {
                        args.next();
                    }
                    "--create-namespace" | "--install" | "--skip-tests" => {}
                    other => {
                        assert!(
                            !other.starts_with('-'),
                            "{PAGE}: {words:?}: an option the kind test does not use: {other}"
                        );
                        out.push(w);
                    }
                }
            }
            out
        };
        assert_eq!(
            positional,
            [&release, &format!("{CHART_REPOSITORY}/{name}")],
            "{PAGE}: {words:?}: the release and the published chart"
        );
        assert_eq!(
            after(words, "--version"),
            Some(version.as_str()),
            "{words:?}"
        );
        assert_eq!(
            after(words, "--namespace"),
            Some(namespace.as_str()),
            "{words:?}"
        );
        if words[1] != "template" {
            assert!(
                words.contains(&"--create-namespace".to_owned()),
                "{words:?}"
            );
        }
        if let Some(values) = after(words, "--values") {
            assert_eq!(values, "values.yaml", "{words:?}: the page's values.yaml");
        }
    }
}

#[test]
fn the_page_values_are_values_of_the_chart() {
    let page = page();
    let values = page_values(&page);
    let chart = read(&format!("{CHART}/values.yaml"));
    let chart_paths = yaml_paths(&chart);
    let free: Vec<String> = chart_paths
        .iter()
        .filter(|(_, v)| v == "{}")
        .map(|(p, _)| format!("{p}."))
        .collect();
    let variables = variables();
    let mut set = 0;
    for (path, _) in yaml_paths(&values) {
        if let Some(name) = path.strip_prefix("config.") {
            assert!(
                variables.contains(name),
                "{PAGE}: config.{name} is not a row of {CONFIGURATION}"
            );
        }
        assert!(
            chart_paths.iter().any(|(p, _)| p == &path) || free.iter().any(|f| path.starts_with(f)),
            "{PAGE}: {path} is not a value of the chart"
        );
        set += 1;
    }
    assert!(set >= 3, "{PAGE}: the values block sets only {set} values");
}

/// The page's collector: (namespace, port) from `otel.endpoint` of its values, `http://<service>.<namespace>:<port>`.
fn collector(values: &str) -> (String, String) {
    let endpoint = yaml_value(values, "otel.endpoint")
        .unwrap_or_else(|| panic!("{PAGE}: the values set no otel.endpoint"));
    let (host, port) = endpoint
        .strip_prefix("http://")
        .and_then(|h| h.split_once(':'))
        .unwrap_or_else(|| panic!("{endpoint}: http://service.namespace:port"));
    let (_, namespace) = host
        .split_once('.')
        .unwrap_or_else(|| panic!("{endpoint}: service.namespace"));
    (namespace.to_owned(), port.trim_end_matches('/').to_owned())
}

/// The `{protocol: P, port: N}` entries of a NetworkPolicy's text, as (protocol, port).
fn policy_ports(text: &str) -> BTreeSet<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim().trim_start_matches("- ").trim_matches(['{', '}']);
            let protocol = l.split("protocol: ").nth(1)?.split(',').next()?.trim();
            let port = l.split("port: ").nth(1)?.trim();
            Some((protocol.to_owned(), port.to_owned()))
        })
        .collect()
}

#[test]
fn the_network_policy_is_the_charts_and_the_page_values_name_the_collector() {
    let page = page();
    let kubernetes = section(&page, "Kubernetes");
    assert!(
        !blocks(kubernetes)
            .iter()
            .any(|(_, b)| b.contains("kind: NetworkPolicy")),
        "{PAGE}: a NetworkPolicy written by hand; the chart's networkPolicy value is the one to use"
    );

    let chart = read(&format!("{CHART}/values.yaml"));
    assert_eq!(
        yaml_value(&chart, "networkPolicy.enabled").as_deref(),
        Some("true"),
        "{CHART}: the NetworkPolicy is on by default, as the page says"
    );
    assert!(
        chart.contains("k8s-app: kube-dns") && page.contains("k8s-app: kube-dns"),
        "the chart's and the page's DNS is the kube-dns pods"
    );
    // Every networkPolicy value the page names is one of the chart's.
    let chart_paths: BTreeSet<String> = yaml_paths(&chart).into_iter().map(|(p, _)| p).collect();
    let named: BTreeSet<&str> = kubernetes
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '.'))
        .filter(|w| w.starts_with("networkPolicy."))
        .map(|w| w.trim_end_matches('.'))
        .collect();
    assert!(named.len() >= 4, "{PAGE}: names only {named:?}");
    for value in named {
        assert!(
            chart_paths.contains(value),
            "{PAGE}: {value} is not a value of the chart"
        );
    }

    // The page's values hold the collector rule to the namespace of the collector they export to.
    let values = page_values(&page);
    let (namespace, port) = collector(&values);
    let otlp_to = yaml_value(&values, "networkPolicy.otlpTo")
        .unwrap_or_else(|| panic!("{PAGE}: the values set no networkPolicy.otlpTo"));
    assert!(
        otlp_to.contains(&format!("kubernetes.io/metadata.name: {namespace}")),
        "{PAGE}: networkPolicy.otlpTo {otlp_to} is not the collector's namespace {namespace}"
    );
    assert!(
        kubernetes.contains(&format!("`{port}` above")),
        "{PAGE}: the Kubernetes section does not give the collector's port {port}"
    );

    // The template: the release's pods, ingress to 8080, egress to DNS and the endpoint's port.
    let template = read(&format!("{CHART}/templates/networkpolicy.yaml"));
    for text in [
        r#"{{- include "docx-to-pdf.selectorLabels" . | nindent 6 }}"#,
        "policyTypes: [Ingress, Egress]",
        "{protocol: TCP, port: 8080}",
        "{protocol: UDP, port: 53}",
        "{protocol: TCP, port: 53}",
        "{protocol: TCP, port: {{ $otlpPort }}}",
    ] {
        assert!(
            template.contains(text),
            "{CHART}: the NetworkPolicy template lacks {text}"
        );
    }
}

#[test]
fn the_kubernetes_section_says_nothing_is_fetched() {
    let page = page();
    let kubernetes = section(&page, "Kubernetes");
    for words in ["linked", "remote template", "OLE link", "no connection"] {
        assert!(
            kubernetes.contains(words),
            "{PAGE}: the Kubernetes section does not mention {words:?}"
        );
    }
}

// --- the page as a page --------------------------------------------------------------------------------------------

#[test]
fn every_relative_link_resolves() {
    let page = page();
    let broken: Vec<String> = link_targets(&page)
        .into_iter()
        .filter(|t| !t.contains("://") && !t.starts_with('#'))
        .filter(|t| {
            !root()
                .join("docs/guide")
                .join(t.split('#').next().unwrap())
                .exists()
        })
        .collect();
    assert!(broken.is_empty(), "{PAGE}: links to nothing: {broken:?}");
}

#[test]
fn every_variable_named_is_one_the_service_reads() {
    let page = page();
    let variables = variables();
    let named: BTreeSet<String> = page
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| w.starts_with("DOCX_TO_PDF_") || w.starts_with("OTEL_"))
        .map(str::to_owned)
        .collect();
    let unknown: Vec<&String> = named.iter().filter(|n| !variables.contains(*n)).collect();
    assert!(
        unknown.is_empty(),
        "{PAGE}: not rows of {CONFIGURATION}: {unknown:?}"
    );
}

#[test]
fn the_yaml_reading_finds_paths_and_lists() {
    let text = "a:\n  b: 1 # c\n  d: [x, y]\n  e:\n    - \"z\"\n    - w\nf: {}\n";
    assert_eq!(
        yaml_paths(text),
        [
            ("a".into(), String::new()),
            ("a.b".into(), "1".into()),
            ("a.d".into(), "[x, y]".into()),
            ("a.e".into(), String::new()),
            ("f".into(), "{}".into()),
        ]
    );
    assert_eq!(yaml_list(text, "a.d"), ["x", "y"]);
    assert_eq!(yaml_list(text, "a.e"), ["z", "w"]);
}

// --- with Docker and helm ------------------------------------------------------------------------------------------

/// The image the Docker tests run in place of the published one.
fn local_image() -> String {
    std::env::var("DOCX_TO_PDF_IMAGE").unwrap_or_else(|_| "docx-to-pdf:dev".to_owned())
}

fn run(program: &str, args: &[&str]) -> Output {
    Command::new(program)
        .args(args)
        .current_dir(root())
        .output()
        .unwrap_or_else(|e| panic!("{program}: {e}"))
}

fn ok(program: &str, args: &[&str]) -> String {
    let out = run(program, args);
    assert!(
        out.status.success(),
        "{program} {args:?}: {}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A compose project of one of the files, the image and port replaced; `down -v` when dropped.
struct Project {
    name: String,
    files: Vec<String>,
    _override: tempfile::TempDir,
}

impl Project {
    fn up(file: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let override_file = dir.path().join("override.yaml");
        std::fs::write(
            &override_file,
            format!(
                "services:\n  docx-to-pdf:\n    image: {}\n    pull_policy: never\n    ports: !override\n      \
                 - \"127.0.0.1::{DEFAULT_PORT}\"\n",
                local_image()
            ),
        )
        .unwrap();
        let stem = file
            .trim_start_matches(&format!("{COMPOSE_DIR}/"))
            .trim_end_matches("compose.yaml")
            .trim_end_matches('/')
            .replace('/', "-");
        let stem = if stem.is_empty() {
            "minimal".to_owned()
        } else {
            stem
        };
        let project = Self {
            name: format!("docx-to-pdf-installation-{stem}-{}", std::process::id()),
            files: vec![
                root().join(file).to_string_lossy().into_owned(),
                override_file.to_string_lossy().into_owned(),
            ],
            _override: dir,
        };
        project.compose(&["up", "--detach"]);
        project
    }

    fn args<'a>(&'a self, rest: &[&'a str]) -> Vec<&'a str> {
        let mut args = vec!["compose", "--project-name", self.name.as_str()];
        for f in &self.files {
            args.extend(["--file", f.as_str()]);
        }
        args.extend(rest);
        args
    }

    fn compose(&self, rest: &[&str]) -> String {
        ok("docker", &self.args(rest))
    }

    /// `127.0.0.1:<port>` the service is published on.
    fn address(&self) -> String {
        self.compose(&["port", "docx-to-pdf", &DEFAULT_PORT.to_string()])
            .trim()
            .to_owned()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = run(
            "docker",
            &self.args(&["down", "--volumes", "--remove-orphans", "--timeout", "30"]),
        );
    }
}

/// Polls until `check` holds, for at most `limit`.
fn wait_for(limit: Duration, what: &str, mut check: impl FnMut() -> bool) {
    let start = Instant::now();
    while !check() {
        assert!(start.elapsed() < limit, "{what}: not within {limit:?}");
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Converts the test document through `address`; panics unless the answer is `200` and a PDF.
fn converts(address: &str) {
    wait_for(Duration::from_secs(120), "/readyz", || {
        run("curl", &["-fsS", &format!("http://{address}/readyz")])
            .status
            .success()
    });
    let out = tempfile::tempdir().unwrap();
    let pdf = out.path().join("out.pdf");
    let status = ok(
        "curl",
        &[
            "-sS",
            "-o",
            pdf.to_str().unwrap(),
            "-w",
            "%{http_code}",
            "-F",
            "document=@tests/corpus/documents/text/text-1p.docx",
            &format!("http://{address}/pdf"),
        ],
    );
    let body = std::fs::read(&pdf).unwrap();
    assert_eq!(status, "200", "{}", String::from_utf8_lossy(&body));
    assert!(body.starts_with(b"%PDF-"), "the answer is a PDF");
}

#[test]
#[ignore = "needs Docker and the image: DOCX_TO_PDF_IMAGE=docx-to-pdf:dev cargo test --test installation -- --ignored"]
fn docker_compose_up_of_the_minimal_file_converts_a_document() {
    let project = Project::up(&format!("{COMPOSE_DIR}/compose.yaml"));
    converts(&project.address());
}

#[test]
#[ignore = "needs Docker and the image: DOCX_TO_PDF_IMAGE=docx-to-pdf:dev cargo test --test installation -- --ignored"]
fn docker_compose_up_with_telemetry_converts_and_the_collector_receives_traces() {
    let project = Project::up(&format!("{COMPOSE_DIR}/telemetry/compose.yaml"));
    converts(&project.address());
    wait_for(Duration::from_secs(60), "the collector's traces", || {
        let logs = project.compose(&["logs", "--no-color", "otel-collector"]);
        logs.lines()
            .any(|l| l.contains("Traces") && l.contains("spans"))
    });
}

#[test]
#[ignore = "needs helm: cargo test --test installation -- --ignored"]
fn helm_template_with_the_page_values_renders_what_the_page_says() {
    let page = page();
    let values = page_values(&page);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("values.yaml");
    std::fs::write(&file, &values).unwrap();
    let release = shell_assignment(&read(KIND_TEST), "RELEASE");
    let namespace = shell_assignment(&read(KIND_TEST), "NS");
    let rendered = ok(
        "helm",
        &[
            "template",
            &release,
            CHART,
            "--namespace",
            &namespace,
            "--values",
            file.to_str().unwrap(),
            "--skip-tests",
        ],
    );

    // The chart's NetworkPolicy selects the pods and lets them reach DNS and the collector, nothing else.
    let policy = rendered
        .split("\n---\n")
        .find(|d| d.contains("\nkind: NetworkPolicy\n"))
        .unwrap_or_else(|| panic!("the chart rendered no NetworkPolicy:\n{rendered}"));
    let selector: Vec<(String, String)> = yaml_paths(policy)
        .into_iter()
        .filter_map(|(p, v)| {
            Some((
                p.strip_prefix("spec.podSelector.matchLabels.")?.to_owned(),
                v,
            ))
        })
        .collect();
    assert_eq!(selector.len(), 2, "{policy}");
    for (label, value) in selector {
        assert!(
            rendered.contains(&format!("        {label}: {value}\n")),
            "the rendered pod template has no label {label}: {value}"
        );
    }
    let (ingress, egress) = policy
        .split_once("\n  egress:\n")
        .unwrap_or_else(|| panic!("no egress:\n{policy}"));
    assert_eq!(
        policy_ports(ingress),
        BTreeSet::from([("TCP".to_owned(), "8080".to_owned())]),
        "ingress to the service port only"
    );
    let (namespace, port) = collector(&values);
    assert_eq!(
        policy_ports(egress),
        BTreeSet::from([
            ("UDP".to_owned(), "53".to_owned()),
            ("TCP".to_owned(), "53".to_owned()),
            ("TCP".to_owned(), port.clone()),
        ]),
        "egress to DNS and the collector's port {port}, nothing else"
    );
    assert_eq!(
        egress.lines().filter(|l| l.trim() == "- ports:").count(),
        2,
        "two egress rules, DNS and the collector:\n{egress}"
    );
    assert!(egress.contains("k8s-app: kube-dns"), "DNS is kube-dns");
    assert!(
        egress.contains(&format!("kubernetes.io/metadata.name: {namespace}")),
        "the collector's namespace {namespace}"
    );
    let endpoint = yaml_value(&values, "otel.endpoint").unwrap();
    assert!(
        rendered.contains(&format!("value: \"{endpoint}\""))
            || rendered.contains(&format!("value: {endpoint}\n")),
        "the rendered pod does not export to {endpoint}"
    );
    let pool: u64 = yaml_value(&values, "poolSize").unwrap().parse().unwrap();
    let hard: u64 = yaml_value(&values, "instanceHardMemoryMiB")
        .map_or(DEFAULT_INSTANCE_HARD_MEMORY / MIB, |v| v.parse().unwrap());
    let memory = 256 + pool * (hard + 256);
    assert!(
        rendered.contains(&format!("memory: {memory}Mi")),
        "the rendered pod does not ask for {memory}Mi"
    );
    assert!(
        page.contains(&format!("{memory} MiB")),
        "{PAGE}: does not give the {memory} MiB its values make"
    );
    assert!(
        !rendered.contains("kind: Pod\n"),
        "--skip-tests leaves the test pod out"
    );
}
