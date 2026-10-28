#!/usr/bin/env ruby
# frozen_string_literal: true

# Runs one job of .gitriver/workflows/ci.yml locally the way a GitRiver runner runs it: in the job's
# image, on a copy of the working tree at /workspace, with the workflow's and the job's `env:` passed through
# unexpanded, each `run:` step as its own `sh -c`, stopping at the first failing step. Nothing is pushed or
# triggered; the container is removed at the end, pass or fail.
#
#   ruby tools/ci/run-job.rb <job> [workflow]      (`make ci-job JOB=<job>`)
#
# What it copies is what a commit of the working tree would hold: tracked and untracked files that are not ignored
# (deleted ones left out), without upstream/docx-to-pdf's contents, which a runner never fetches. A fresh git index
# records the submodule's gitlink, so the jobs' own `git submodule update` fetches it as on a runner. A job with
# `needs:` is run alone: `needs` outputs only name caches, and caches are not emulated (every run is cold).
#
# A job with `runs-on:` (the `image` job, on the runner that gives its jobs the host's Docker socket) is refused: this
# container has no socket, and the job's local form is `make image-ci` on the host.
#
# Exit status: the failing step's, 0 when every step passed, 2 on a usage or setup error.
#
# Ruby stdlib only, so it runs on the host's Ruby (2.6 on macOS), as tools/ci/lint-gitriver-workflows does.

require "psych"
require "date"
require "open3"

job_name = ARGV[0]
workflow = ARGV[1] || ".gitriver/workflows/ci.yml"
abort "usage: ruby tools/ci/run-job.rb <job> [workflow]" if job_name.nil? || job_name.empty?

doc = Psych.safe_load(File.read(workflow), permitted_classes: [Date, Time, Symbol], aliases: true)
job = doc.fetch("jobs").fetch(job_name) do
  warn "no job '#{job_name}' in #{workflow}; jobs: #{doc['jobs'].keys.join(', ')}"
  exit 2
end
image = job["image"]
unless image.is_a?(String) && !image.empty?
  warn "job '#{job_name}' names no string image (tools/ci/lint-gitriver-workflows)"
  exit 2
end

if job["runs-on"]
  warn "job '#{job_name}' runs on #{Array(job['runs-on']).join(', ')}, a runner with the host's Docker socket; " \
       "run it on the host as `make image-ci`"
  exit 2
end

env = (doc["env"] || {}).merge(job["env"] || {}).transform_values(&:to_s)
env["CI_PROJECT_DIR"] = "/workspace"
env["CI_OUTPUT"] = "/tmp/ci-output"
submodule = "upstream/docx-to-pdf"
gitlink = `git ls-files -s -- #{submodule}`.split[1]

def sh!(*cmd)
  ok = system(*cmd)
  return if ok

  warn "failed: #{cmd.join(' ')}"
  exit 2
end

files = `git ls-files -z -co --exclude-standard`.split("\0").uniq
files.reject! { |f| f == submodule || f.start_with?("#{submodule}/") || !File.file?(f) }

name = "docx-to-pdf-ci-#{job_name}-#{Process.pid}"
status = 2
begin
  env_args = env.flat_map { |k, v| ["-e", "#{k}=#{v}"] }
  sh!("docker", "run", "-d", "--name", name, "-w", "/workspace", *env_args, "--entrypoint", "sh", image,
      "-c", "mkdir -p /workspace && exec sleep 86400", out: File::NULL)
  Open3.popen2("docker", "cp", "-", "#{name}:/workspace") do |stdin, out, wait|
    # COPYFILE_DISABLE: macOS tar would add an AppleDouble `._` file beside each file with extended attributes.
    # --no-xattrs: it would also carry them as pax headers, and `docker cp` fails on one the container's filesystem
    # does not support (`lsetxattr /workspace/Cargo.lock: xattr "com.apple.provenance"`).
    tar_out, tar_status = Open3.capture2({ "COPYFILE_DISABLE" => "1" }, "tar", "--no-xattrs", "-c", "-f", "-",
                                         "--null", "-T", "-", stdin_data: files.join("\0"), binmode: true)
    abort "tar failed" unless tar_status.success?
    stdin.binmode
    stdin.write(tar_out)
    stdin.close
    out.read
    abort "docker cp failed" unless wait.value.success?
  end
  # A fresh index with the submodule's gitlink, in an image that has git; nothing is committed.
  if gitlink
    system("docker", "exec", name, "sh", "-c",
           "command -v git > /dev/null || exit 0; git config --global --add safe.directory '*' && git init -q && " \
           "git update-index --add --cacheinfo 160000,#{gitlink},#{submodule}")
  end
  puts "job '#{job_name}' in #{image} as #{name}, #{files.size} files"
  status = 0
  (job["steps"] || []).each_with_index do |step, i|
    run = step["run"]
    next unless run

    puts "--- step #{i + 1}: #{run.lines.first.strip}#{run.lines.size > 1 ? ' …' : ''}"
    started = Time.now
    system("docker", "exec", name, "sh", "-c", run)
    code = $?.exitstatus || 1
    puts "--- step #{i + 1}: exit #{code} after #{(Time.now - started).round}s"
    next if code.zero?

    status = code
    break
  end
ensure
  system("docker", "rm", "-f", "-v", name, out: File::NULL, err: File::NULL)
end
puts "job '#{job_name}': #{status.zero? ? 'passed' : "failed (#{status})"}"
exit status
