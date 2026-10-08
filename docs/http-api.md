# HTTP API 契约

本页记录已确认的最小闭环 API 契约；当前空程序不提供这些接口。备份和恢复异步执行，运行途中可发起中止。运行与配置边界见 [C2](architecture/containers.md)。

所有 `/api` 请求使用 `Authorization: Bearer <secret_key>`；有 JSON 请求体时使用 `Content-Type: application/json`。页面和静态资源不需要 key。API 路由和错误不回退为控制台 HTML，响应不包含访问凭据。

## 路由与字段

| 方法与路径 | 请求 | 成功响应 |
|---|---|---|
| `GET /api/status` | 无请求体。 | `200`，`version`、`active_job_id`；空闲时运行 ID 为 `null`。 |
| `POST /api/backups` | JSON：`source`、`repository`。 | `202`，`job_id`、`kind`、`state`，表示已接收备份运行。 |
| `GET /api/backups?repository=<PATH>` | 查询参数 `repository`，按 URL 规则编码。 | `200`，`backups` 数组，每项包含 `backup_id` 和 `created_at`；仅列出正式备份。 |
| `POST /api/restores` | JSON：`repository`、`backup_id`、`destination`。 | `202`，`job_id`、`kind`、`state`，表示已接收恢复运行。 |
| `GET /api/jobs/{job_id}` | 路径中的运行 ID。 | `200`，运行状态、进度，以及成功结果或失败原因。 |
| `POST /api/jobs/{job_id}/cancel` | 无请求体，发起中止。 | `202`，`job_id`、`kind`、`state: "cancelling"`；查询运行确认实际停止。 |
| `POST /api/config/reload` | 无请求体，重新读取工作目录下的 `config.json`。 | `202`，`job_id`、`kind`、`state`，通过运行查询确认是否生效。 |

配置重载也使用运行 ID，以便查询下载与解压的结果。`202` 只表示请求已接收，不代表备份、恢复或重载成功，符合 [HTTP 语义规范](https://www.rfc-editor.org/rfc/rfc9110.html#section-15.3.3)。响应同时提供 `Location: /api/jobs/{job_id}` 作为查询地址。

`source` 是源目录，`repository` 是备份仓库目录，`destination` 是恢复目录；均指向服务器文件系统。相对路径统一以工作目录为基准。备份仓库必须位于源目录之外，避免把备份产物再次纳入源；恢复位置不能位于备份仓库内。第一阶段恢复到不存在或为空的目录，不覆盖已有内容，也不提供部分恢复。

`backup_id` 为服务端生成的备份标识，不是任意文件路径。列表为空时返回 `{"backups": []}`；仓库损坏或读取失败应返回错误，不假装为空。

## 备份与恢复示例

创建备份的请求体：

```json
{
  "source": "source",
  "repository": "repository"
}
```

接收后返回：

```json
{
  "job_id": "job-001",
  "kind": "backup",
  "state": "running"
}
```

成功后，使用 `GET /api/jobs/job-001` 获取正式备份的 `backup_id`。再创建恢复：

```json
{
  "repository": "repository",
  "backup_id": "backup-001",
  "destination": "restored"
}
```

这些 ID 仅用于示例；具体生成方式在实现时确定。时间字段使用 UTC 的 RFC 3339 字符串。

## 运行状态与结果

`kind` 为 `backup`、`restore` 或 `config_reload`。第一版不排队：正常运行从 `running` 进入 `succeeded` 或 `failed`，主动中止从 `running` 经 `cancelling` 进入 `cancelled`。同一服务器已有 `running` 或 `cancelling` 运行时拒绝新运行。备份和恢复的 `progress` 包含已完成文件数 `files_completed` 与这些文件的原始字节数 `bytes_completed`，不承诺百分比；配置重载的 `progress` 为 `null`。

运行中 `result` 与 `error` 都为 `null`。成功时 `error` 为 `null`，备份结果包含 `backup_id`，恢复结果包含 `destination`，重载结果为 `{"reloaded": true}`。失败时 `result` 为 `null`，`error` 包含 `code` 和 `message`。例如：

```json
{
  "job_id": "job-001",
  "kind": "backup",
  "state": "succeeded",
  "progress": {
    "files_completed": 2,
    "bytes_completed": 1024
  },
  "result": {
    "backup_id": "backup-001"
  },
  "error": null
}
```

备份只有通过校验并发布后才能标为成功；恢复必须完成全部条目并通过校验。执行阶段的错误保存在运行的 `error` 中，查询这个失败运行仍返回 `200`，因为查询本身成功。

第一版运行记录只在当前进程内可查询，重启后未知运行 ID 返回 `404`；不把丢失记录推断为成功或失败。客户端超时后应查询已有运行，不自动重发创建请求；本轮暂不增加请求幂等键、续跑或持久化调度。

## 中止运行

备份、恢复与配置重载运行途中均可通过 `POST /api/jobs/{job_id}/cancel` 发起中止。服务器接收请求后返回 `202` 和 `cancelling` 状态；客户端随后查询运行，直到 `cancelled` 或其他最终状态。重复中止一个 `cancelling` 运行仍返回 `202`，已 `cancelled` 则返回 `200` 和当前状态；未知 ID 返回 `404`，已成功或失败的运行返回 `409`。

中止是协作式停止，不强行终止线程或进程。服务器在遍历文件、流式读写、压缩、校验以及下载期间检查中止请求，并在实际停止和临时数据清理后标记 `cancelled`；不能只设置状态而让工作继续。中止期间若发生执行或清理错误则标记 `failed`，保留原因。正常中止的 `result` 与 `error` 均为 `null`，进度保留停止时的值。

中止备份不发布正式备份；中止恢复不继续放置文件，已经校验并放置的文件保留，不自动删除整个恢复目录。中止配置重载保留旧配置与控制台。接受中止与最终发布按顺序判定：中止先被接受则不得发布；最终提交先完成则运行已成功，中止请求返回 `409`。这些规则需要在实现时验证竞争和中断情况。

## 请求错误

请求未被接受时直接返回相应 HTTP 状态，响应采用以下格式：

```json
{
  "error": {
    "code": "invalid_request",
    "message": "source is required"
  }
}
```

| 状态 | 使用场景 |
|---|---|
| `400` | JSON、字段或路径参数无效；配置重载中读取到的无效配置则记录为运行失败。 |
| `401` | key 缺失或无效，返回 `WWW-Authenticate: Bearer`。 |
| `404` | 路由、运行 ID 或提交时可确定不存在的资源。 |
| `409` | 服务器忙，或提交时可确定恢复位置非空等状态冲突。 |
| `415` | 请求体不是所要求的 JSON 内容类型。 |
| `500` | 服务端未能处理请求；消息须包含定位问题所需的上下文。 |

`code` 供程序判断，`message` 解释具体原因，并避免包含 secret key。进入运行后发生的下载、解压、读写、校验或配置冲突均使运行进入 `failed`，不改写为一次新的 HTTP 响应。更换 key 的重载成功后，后续查询使用新 key。
