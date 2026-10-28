#!/usr/bin/env ruby
# frozen_string_literal: true

# Holds CI to `make lint`: every gate of tools/lint/lint.sh has a `lint-<gate>` job in the workflow
# whose image is exactly the gate's pinned ref in tools/lint/images.Dockerfile and whose steps call
# `make lint-run GATE=<gate>`, and no `lint-*` job runs anything that is not a gate. So a tool bump in
# images.Dockerfile, a new gate, or a job that grows its own copy of a command fails here until the two agree.
#
#   ruby tools/lint/check-pins.rb [workflow]      (default .github/workflows/ci.yml)
#
# The `workflows` gate runs it on .gitriver/workflows/ci.yml and on the default: the two CI definitions carry the
# same jobs, so a gate cannot drift between them.
#
# The default workflow not existing yet is a clean skip (exit 0, saying so); a named one that is missing is exit 2.
# Exit 0: they agree. Exit 1: findings, one line each. Exit 2: lint.sh or the workflow cannot be read.
# Ruby stdlib only.

require "psych"
require "date"

DEFAULT_WORKFLOW = ".github/workflows/ci.yml"
workflow = ARGV[0] ? File.expand_path(ARGV[0]) : DEFAULT_WORKFLOW
Dir.chdir(File.expand_path("../..", __dir__))
if ARGV[0].nil? && !File.file?(workflow)
  puts "check-pins: no #{DEFAULT_WORKFLOW} yet, nothing to hold to the gates (skipped)"
  exit 0
end
unless File.file?(workflow)
  warn "check-pins: no workflow at #{workflow}"
  exit 2
end

list = `sh tools/lint/lint.sh --list`
unless $?.success?
  warn "check-pins: `sh tools/lint/lint.sh --list` failed"
  exit 2
end
gates = list.lines.to_h do |line|
  gate, _stage, ref = line.split
  [gate, ref]
end

doc = Psych.safe_load(File.read(workflow), permitted_classes: [Date, Time, Symbol], aliases: true)
jobs = doc.is_a?(Hash) ? doc["jobs"] : nil
unless jobs.is_a?(Hash)
  warn "check-pins: #{workflow} has no `jobs:`"
  exit 2
end

findings = []
gates.each do |gate, ref|
  name = "lint-#{gate}"
  job = jobs[name]
  if job.nil?
    findings << "#{workflow}: no job `#{name}` for the gate `#{gate}`"
    next
  end
  # GitRiver names a job's image `image:`; GitHub Actions `container:`, a string or a mapping with `image:`.
  container = job["container"]
  image = job.key?("image") ? job["image"] : (container.is_a?(Hash) ? container["image"] : container)
  if image != ref
    findings << "#{workflow}: job `#{name}` runs in `#{image}`, tools/lint/images.Dockerfile pins `#{ref}`"
  end
  runs = (job["steps"] || []).map { |s| s.is_a?(Hash) ? s["run"].to_s : "" }
  unless runs.any? { |r| r.match?(/\bmake lint-run GATE=#{Regexp.escape(gate)}(\s|$)/) }
    findings << "#{workflow}: job `#{name}` does not call `make lint-run GATE=#{gate}`"
  end
end
(jobs.keys.grep(/\Alint-/) - gates.keys.map { |g| "lint-#{g}" }).each do |name|
  findings << "#{workflow}: job `#{name}` is no gate of tools/lint/lint.sh (`sh tools/lint/lint.sh --list`)"
end

findings.each { |f| puts f }
puts "check-pins: #{gates.size} gates, #{findings.size} findings" if findings.empty?
exit(findings.empty? ? 0 : 1)
