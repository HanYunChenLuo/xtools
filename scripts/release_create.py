#!/usr/bin/env python3
"""创建 GitLab Release（描述取 CHANGELOG.md 对应 tag 章节）。

由 .gitlab-ci.yml 的 release job 在 tag 流水线调用（JOB-TOKEN 认证）；
也可本地执行（GITLAB_TOKEN，如 CI 通道故障时的补偿路径）。

幂等：Release 已存在（HTTP 409）时保持现状退出 0。

环境变量：同 scripts/release_upload.py（RELEASE_TAG/CI_COMMIT_TAG、
GITLAB_TOKEN 或 CI_JOB_TOKEN、GITLAB_API/CI_API_V4_URL、
GITLAB_PROJECT_ID/CI_PROJECT_ID）。
"""

from __future__ import annotations

import json
import os
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from release_upload import DEFAULT_PROJECT_ID, api, env


def main() -> None:
    tag = env("RELEASE_TAG", "CI_COMMIT_TAG")
    project_id = env("GITLAB_PROJECT_ID", "CI_PROJECT_ID", default=DEFAULT_PROJECT_ID)
    if not tag:
        sys.exit("缺少 RELEASE_TAG（或 CI_COMMIT_TAG）")
    if os.environ.get("GITLAB_TOKEN"):
        auth = ("PRIVATE-TOKEN", os.environ["GITLAB_TOKEN"])
    elif os.environ.get("CI_JOB_TOKEN"):
        auth = ("JOB-TOKEN", os.environ["CI_JOB_TOKEN"])
    else:
        sys.exit("缺少认证：GITLAB_TOKEN 或 CI_JOB_TOKEN")

    text = Path("CHANGELOG.md").read_text(encoding="utf-8")
    m = re.search(rf"^## \[{re.escape(tag)}\][^\n]*\n(.*?)(?=^## \[|\Z)", text, re.M | re.S)
    if not m:
        sys.exit(f"CHANGELOG.md 缺少 {tag} 章节")
    description = m.group(1).strip()

    status, resp = api(
        "POST",
        f"/projects/{project_id}/releases",
        data={"name": f"Release {tag}", "tag_name": tag, "description": description},
        auth=auth,
    )
    if status == 201:
        print(f"Release {tag} created（描述 {len(description)} 字符）")
    elif status == 409:
        print(f"Release {tag} 已存在，保持现状")
    else:
        sys.exit(f"创建 Release 失败: HTTP {status} {json.dumps(resp, ensure_ascii=False)}")


if __name__ == "__main__":
    main()
