"""Loopback-only ONVIF Media1/Media2 fixture for the opt-in Swift tests."""

from base64 import b64decode
from hashlib import md5, sha1
import re
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from xml.etree import ElementTree

MEDIA1 = "http://www.onvif.org/ver10/media/wsdl"
MEDIA2 = "http://www.onvif.org/ver20/media/wsdl"
DEVICE = "http://www.onvif.org/ver10/device/wsdl"
TOKEN_TYPE = (
    "http://docs.oasis-open.org/wss/2004/01/"
    "oasis-200401-wss-username-token-profile-1.0#PasswordDigest"
)


def profile(token, width, height, namespace, encoding="H264"):
    resolution = (
        f"<t:Resolution><t:Width>{width}</t:Width>"
        f"<t:Height>{height}</t:Height></t:Resolution>"
    )
    if namespace == MEDIA1:
        configuration = (
            f'<t:VideoEncoderConfiguration token="{token}-encoder">'
            f"<t:Encoding>{encoding}</t:Encoding>{resolution}"
            "</t:VideoEncoderConfiguration>"
        )
    else:
        configuration = (
            "<m:Configurations>"
            f'<t:VideoEncoder token="{token}-encoder">'
            f"<t:Encoding>{encoding}</t:Encoding>{resolution}</t:VideoEncoder>"
            "</m:Configurations>"
        )
    return f'<m:Profiles token="{token}">{configuration}</m:Profiles>'


class SOAP(BaseHTTPRequestHandler):
    def authenticated(self):
        header = self.headers.get("Authorization", "")
        if not header.startswith("Digest "):
            return False
        fields = {
            match[0]: match[1] or match[2]
            for match in re.findall(r'(\w+)=(?:"([^"]*)"|([^,\s]+))', header)
        }
        assert fields["username"] == "viewer"
        assert fields["uri"] == self.path
        assert fields["qop"] == "auth"
        ha1 = md5(b"viewer:CamCam:fixture-password").hexdigest()
        ha2 = md5(f"POST:{self.path}".encode()).hexdigest()
        expected = md5(
            f'{ha1}:fixture-http-nonce:{fields["nc"]}:{fields["cnonce"]}:auth:{ha2}'.encode()
        ).hexdigest()
        return fields["response"] == expected

    def do_POST(self):
        if self.path == "/digest" and not self.authenticated():
            self.send_response(401)
            self.send_header(
                "WWW-Authenticate",
                'Digest realm="CamCam", nonce="fixture-http-nonce", qop="auth", algorithm=MD5',
            )
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        root = ElementTree.fromstring(
            self.rfile.read(int(self.headers["Content-Length"]))
        )
        fields = {node.tag.split("}")[-1]: node.text for node in root.iter()}
        assert fields["Username"] == "viewer"
        expected_digest = sha1(
            b64decode(fields["Nonce"])
            + (fields["Created"] + "fixture-password").encode()
        ).digest()
        assert b64decode(fields["Password"]) == expected_digest
        assert root.find(".//{*}Password").attrib["Type"] == TOKEN_TYPE

        action = self.headers["Content-Type"].split("/")[-1].strip('"')
        namespace = MEDIA1 if self.path.endswith("media1") else MEDIA2
        if action == "GetServices":
            if self.path == "/legacy":
                service, path = MEDIA1, "media1"
            else:
                service, path = MEDIA2, "media2"
            body = (
                f"<m:Service><m:Namespace>{service}</m:Namespace>"
                f"<m:XAddr>http://127.0.0.1:18080/{path}</m:XAddr></m:Service>"
            )
            namespace = DEVICE
        elif action == "GetProfiles":
            body = profile("main", 1280, 720, namespace)
            body += profile("sub", 480, 270, namespace)
            body += profile("jpeg", 160, 90, namespace, encoding="JPEG")
        elif action == "GetStreamUri":
            token = fields["ProfileToken"]
            assert token in ("sub", "main")
            assert fields["Protocol"] == "RTSP"
            body = f"<m:Uri>rtsp://127.0.0.1:18554/{token}</m:Uri>"
        else:
            self.send_error(400)
            return

        data = (
            '<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" '
            f'xmlns:m="{namespace}" xmlns:t="http://www.onvif.org/ver10/schema">'
            f"<s:Body><m:{action}Response>{body}</m:{action}Response>"
            "</s:Body></s:Envelope>"
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/soap+xml")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", 18080), SOAP).serve_forever()
