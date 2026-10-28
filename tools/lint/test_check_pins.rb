# frozen_string_literal: true

# check-pins.rb on throwaway trees: a stub lint.sh with one gate, and workflows that keep or break its rules.
#
#   ruby tools/lint/test_check_pins.rb      (the `workflows` gate runs it; Ruby stdlib and minitest only)

require "minitest/autorun"
require "fileutils"
require "open3"
require "tmpdir"

class CheckPinsTest < Minitest::Test
  SCRIPT = File.expand_path("check-pins.rb", __dir__)
  IMAGE = "rust:1@sha256:#{"a" * 64}"
  CHECKOUT = "actions/checkout@#{"0" * 40}  # v7.0.1"

  def gate_job(uses = CHECKOUT)
    <<~YAML
      jobs:
        lint-fmt:
          runs-on: ubuntu-24.04
          container: #{IMAGE}
          permissions:
            contents: read
          steps:
            - uses: #{uses}
            - run: make lint-run GATE=fmt
    YAML
  end

  # Runs check-pins.rb in a tree holding `files` (path => content); returns [exit code, output].
  def run_in(files)
    Dir.mktmpdir do |root|
      FileUtils.mkdir_p(File.join(root, "tools/lint"))
      FileUtils.cp(SCRIPT, File.join(root, "tools/lint/check-pins.rb"))
      File.write(File.join(root, "tools/lint/lint.sh"), "echo 'fmt rust #{IMAGE}'\n")
      files.each do |path, content|
        FileUtils.mkdir_p(File.dirname(File.join(root, path)))
        File.write(File.join(root, path), content)
      end
      out, status = Open3.capture2e("ruby", File.join(root, "tools/lint/check-pins.rb"))
      [status.exitstatus, out]
    end
  end

  def test_a_hardened_workflow_passes
    code, out = run_in(".github/workflows/ci.yml" => "permissions: {}\n#{gate_job}")
    assert_equal 0, code, out
  end

  def test_no_workflow_is_a_skip
    code, out = run_in({})
    assert_equal 0, code, out
    assert_match(/skipped/, out)
  end

  def test_permissions_at_the_top_must_be_empty
    code, out = run_in(".github/workflows/ci.yml" => "permissions:\n  contents: read\n#{gate_job}")
    assert_equal 1, code
    assert_match(/ci\.yml: no `permissions: \{\}` at the top/, out)
  end

  def test_missing_permissions_fail_in_any_workflow
    code, out = run_in(
      ".github/workflows/ci.yml" => "permissions: {}\n#{gate_job}",
      ".github/workflows/other.yml" => "on: push\njobs: {}\n",
    )
    assert_equal 1, code
    assert_match(/other\.yml: no `permissions: \{\}`/, out)
  end

  def test_a_tag_is_no_pin
    code, out = run_in(".github/workflows/ci.yml" => "permissions: {}\n#{gate_job("actions/checkout@v7")}")
    assert_equal 1, code
    assert_match(/ci\.yml:\d+: `actions\/checkout@v7` is not pinned by its full commit/, out)
  end

  def test_a_pin_needs_its_release_in_a_comment
    code, out = run_in(".github/workflows/ci.yml" => "permissions: {}\n#{gate_job("actions/checkout@#{"0" * 40}")}")
    assert_equal 1, code
    assert_match(/has no `# v<release>` comment/, out)
  end

  def test_a_docker_action_needs_a_digest
    code, out = run_in(".github/workflows/ci.yml" => "permissions: {}\n#{gate_job("docker://alpine:3")}")
    assert_equal 1, code
    assert_match(/`docker:\/\/alpine:3` is not pinned by digest/, out)
  end

  def test_a_local_workflow_needs_no_pin
    local = "permissions: {}\n#{gate_job}  site:\n    uses: ./.github/workflows/site.yml\n"
    code, out = run_in(".github/workflows/ci.yml" => local)
    assert_equal 0, code, out
  end

  def test_the_gates_still_hold
    code, out = run_in(".github/workflows/ci.yml" => "permissions: {}\njobs:\n  lint-other:\n    steps: []\n")
    assert_equal 1, code
    assert_match(/no job `lint-fmt`/, out)
    assert_match(/job `lint-other` is no gate/, out)
  end
end
