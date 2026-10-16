#!/usr/bin/env python3
"""把 kiro-cli 的登录凭据桥接成 kiro2api 能读的 SSO 缓存文件。

背景：新版 kiro-cli（>= 2.31）把凭据存在自己的密钥库里
（macOS: ~/Library/Application Support/kiro-cli/data.sqlite3 的 auth_kv 表，
并镜像一份到钥匙串），不再写 Kiro IDE 用的 ~/.aws/sso/cache/kiro-auth-token.json。
本脚本把 CLI 凭据映射成 IDE 格式（含 client registration 文件，续期必需），
落盘后 kiro2api 即可读取并自动续期。

只读 CLI 侧数据（sqlite 以 mode=ro 打开），不改动 kiro-cli 的任何文件。
用法：python3 scripts/bridge-kiro-cli-credentials.py
"""
import json
import os
import sqlite3
import sys

DB = os.path.expanduser("~/Library/Application Support/kiro-cli/data.sqlite3")
SSO = os.path.expanduser("~/.aws/sso/cache")

TOKEN_KEYS = ("kirocli:odic:token", "kirocli:external-idp:token")
REGISTRATION_KEY = "kirocli:odic:device-registration"


def load_kv(con, key):
    row = con.execute("select value from auth_kv where key = ?", (key,)).fetchone()
    if row is None:
        return None
    try:
        return json.loads(row[0])
    except json.JSONDecodeError as e:
        sys.exit(f"错误：auth_kv[{key!r}] 不是合法 JSON：{e}")


def main():
    if not os.path.isfile(DB):
        sys.exit(f"错误：找不到 kiro-cli 凭据库 {DB}（是否已安装并 kiro-cli login？）")
    # URI 只读打开，避免误写 CLI 的库；路径含空格需转成 URI
    from pathlib import Path
    uri = Path(DB).as_uri() + "?mode=ro"
    con = sqlite3.connect(uri, uri=True)
    try:
        tok = next((t for t in (load_kv(con, k) for k in TOKEN_KEYS) if t), None)
        reg = load_kv(con, REGISTRATION_KEY)
    finally:
        con.close()

    if tok is None:
        sys.exit(f"错误：{DB} 里没有令牌记录（是否已 kiro-cli login？）")

    # social（Builder ID / 外部 IdP）登录没有 device registration，
    # kiro2api 走 social 刷新链路，不需要注册文件
    is_idc = bool(tok.get("start_url"))
    if is_idc and reg is None:
        sys.exit("错误：IdC 登录但缺少 device-registration 记录，无法支撑续期。")

    os.makedirs(SSO, exist_ok=True)
    try:
        os.chmod(SSO, 0o700)
    except OSError:
        pass

    if is_idc:
        import hashlib
        # 缓存文件名沿用 AWS CLI 惯例：client_id 的 sha256，稳定且不与 IDE 的冲突
        hash_name = hashlib.sha256(reg["client_id"].encode()).hexdigest()
        write(os.path.join(SSO, hash_name + ".json"), {
            "clientId": reg["client_id"],
            "clientSecret": reg["client_secret"],
            "expiresAt": reg["client_secret_expires_at"],
        })

    token_payload = {
        "accessToken": tok["access_token"],
        "refreshToken": tok["refresh_token"],
        "expiresAt": tok["expires_at"],
        "region": tok.get("region", "us-east-1"),
    }
    if is_idc:
        token_payload.update({"authMethod": "IdC", "provider": None, "clientIdHash": hash_name})
    else:
        token_payload.update({"authMethod": "social", "provider": tok.get("provider")})
    write(os.path.join(SSO, "kiro-auth-token.json"), token_payload)

    print("完成。在 kiro2api 里点「重新检测」或重启代理即可。")
    print("注意：kiro-cli 重新登录后需重跑本脚本同步最新凭据。")


def write(path, payload):
    import tempfile
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), prefix=".bridge-")
    try:
        with os.fdopen(fd, "w") as f:
            json.dump(payload, f, indent=2)
            f.write("\n")
        os.chmod(tmp, 0o600)
        os.replace(tmp, path)
    except BaseException:
        if os.path.exists(tmp):
            os.unlink(tmp)
        raise
    print("written:", path)


if __name__ == "__main__":
    main()
