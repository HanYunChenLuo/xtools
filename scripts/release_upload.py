#!/usr/bin/env python3
"""上传一个发布产物到 GitLab package registry 并挂为 Release 资产链接。

CI（release job）与本地（scripts/release-macos.sh）共用。幂等：同名资产链接先删后建，
package registry 同版本同名文件会追加新文件（旧文件仍在，链接指向最新上传的）。

用法：
    python3 scripts/release-upload.py <tar.gz 路径> <资产链接名>

环境变量（CI 内大多由预定义变量自动满足，本地只需 RELEASE_TAG + GITLAB_TOKEN）：
    RELEASE_TAG          必填（或由 CI_COMMIT_TAG 提供）——如 v0.2.0
    GITLAB_TOKEN         Personal Access Token（api 权限，PRIVATE-TOKEN 认证）
    CI_JOB_TOKEN         无 GITLAB_TOKEN 时启用（CI 内自动，JOB-TOKEN 认证）
    GITLAB_API           选填，默认 https://gitlab.chehejia.com/api/v4（CI 内用 CI_API_V4_URL）
    GITLAB_PROJECT_ID    选填，默认 39859（ligraphic/xtools；CI 内用 CI_PROJECT_ID）
    GITLAB_PROJECT_URL   选填，项目 web 地址，默认 https://gitlab.chehejia.com/ligraphic/xtools
"""

from __future__ import annotations

import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

DEFAULT_API = "https://gitlab.chehejia.com/api/v4"
DEFAULT_PROJECT_ID = "39859"  # ligraphic/xtools
DEFAULT_PROJECT_URL = "https://gitlab.chehejia.com/ligraphic/xtools"


def env(*names: str, default: str = "") -> str:
    for n in names:
        if os.environ.get(n):
            return os.environ[n]
    return default


def api(method: str, path: str, *, data=None, raw: bytes | None = None,
        auth: tuple[str, str] = ("", "")) -> tuple[int, dict]:
    url = f"{env('GITLAB_API', 'CI_API_V4_URL', default=DEFAULT_API)}{path}"
    headers = {auth[0]: auth[1]}
    body = raw
    if data is not None:
        body = json.dumps(data).encode()
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=body, method=method, headers=headers)
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"{}")


def main() -> None:
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    file_path, link_name = Path(sys.argv[1]), sys.argv[2]
    tag = env("RELEASE_TAG", "CI_COMMIT_TAG")
    project_id = env("GITLAB_PROJECT_ID", "CI_PROJECT_ID", default=DEFAULT_PROJECT_ID)
    project_url = env("GITLAB_PROJECT_URL", "CI_PROJECT_URL", default=DEFAULT_PROJECT_URL)
    if not tag:
        sys.exit("缺少 RELEASE_TAG（或 CI_COMMIT_TAG）")
    if not file_path.is_file():
        sys.exit(f"产物不存在: {file_path}")
    if os.environ.get("GITLAB_TOKEN"):
        auth = ("PRIVATE-TOKEN", os.environ["GITLAB_TOKEN"])
    elif os.environ.get("CI_JOB_TOKEN"):
        auth = ("JOB-TOKEN", os.environ["CI_JOB_TOKEN"])
    else:
        sys.exit("缺少认证：GITLAB_TOKEN 或 CI_JOB_TOKEN")

    # 1) 上传 package registry（generic 仓库 xtools/<tag>/<file>）
    quoted = urllib.parse.quote(file_path.name, safe="")
    status, resp = api(
        "PUT",
        f"/projects/{project_id}/packages/generic/xtools/{tag}/{quoted}",
        raw=file_path.read_bytes(), auth=auth,
    )
    if status not in (200, 201):
        sys.exit(f"上传 {file_path.name} 失败: HTTP {status} {json.dumps(resp, ensure_ascii=False)}")
    file_id = resp["package_files"][0]["id"]
    file_url = f"{project_url}/-/package_files/{file_id}"
    print(f"uploaded {file_path.name} -> {file_url}")

    # 2) 挂资产链接（同名先删，幂等重跑）
    status, links = api("GET", f"/projects/{project_id}/releases/{tag}/assets/links",
                        auth=auth)
    if status == 200:
        for old in links:
            if old["name"] == link_name:
                api("DELETE",
                    f"/projects/{project_id}/releases/{tag}/assets/links/{old['id']}",
                    auth=auth)
                print(f"removed old link '{link_name}' (id={old['id']})")
    status, resp = api("POST", f"/projects/{project_id}/releases/{tag}/assets/links",
                       data={"name": link_name, "url": file_url, "link_type": "package"},
                       auth=auth)
    if status not in (200, 201):
        sys.exit(f"挂链接 '{link_name}' 失败: HTTP {status} {json.dumps(resp, ensure_ascii=False)}")
    print(f"linked '{link_name}' -> Release {tag}")


if __name__ == "__main__":
    main()
