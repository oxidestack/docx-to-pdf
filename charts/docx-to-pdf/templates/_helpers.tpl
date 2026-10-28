{{- define "docx-to-pdf.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "docx-to-pdf.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{- define "docx-to-pdf.selectorLabels" -}}
app.kubernetes.io/name: {{ include "docx-to-pdf.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{- define "docx-to-pdf.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{ include "docx-to-pdf.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{- define "docx-to-pdf.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "docx-to-pdf.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{- define "docx-to-pdf.image" -}}
{{- if .Values.image.digest }}
{{- printf "%s@%s" .Values.image.repository .Values.image.digest }}
{{- else }}
{{- printf "%s:%s" .Values.image.repository (default .Chart.AppVersion .Values.image.tag) }}
{{- end }}
{{- end }}

{{/* The tmpfs's size counts against the pod's memory; an on-disk /tmp does not. */}}
{{- define "docx-to-pdf.tmpfsMiB" -}}
{{- if eq .Values.tmp.medium "Memory" }}
{{- if not (gt (int .Values.tmp.sizeMiB) 0) }}
{{- fail "tmp.medium Memory needs tmp.sizeMiB: the tmpfs counts against the memory limit (migration guide section 4)" }}
{{- end }}
{{- int .Values.tmp.sizeMiB }}
{{- else }}
{{- 0 }}
{{- end }}
{{- end }}

{{/* docs/guide/operations.md, "Sizing": 256 + POOL_SIZE × (H + 256) MiB, plus the tmpfs. */}}
{{- define "docx-to-pdf.memoryMiB" -}}
{{- $pool := int .Values.poolSize }}
{{- $hard := int .Values.instanceHardMemoryMiB }}
{{- if not (gt $hard 0) }}
{{- fail "instanceHardMemoryMiB 0 (no hard limit) leaves the memory limit unbounded: set resources yourself" }}
{{- end }}
{{- add 256 (mul $pool (add $hard 256)) (include "docx-to-pdf.tmpfsMiB" . | int) }}
{{- end }}

{{- define "docx-to-pdf.resources" -}}
{{- if .Values.resources }}
{{- toYaml .Values.resources }}
{{- else }}
{{- $memory := printf "%sMi" (include "docx-to-pdf.memoryMiB" .) }}
requests:
  cpu: {{ .Values.poolSize | quote }}
  memory: {{ $memory }}
limits:
  memory: {{ $memory }}
{{- end }}
{{- end }}
