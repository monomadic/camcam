"""Loopback WS-Discovery responder for CamCam's multicast discovery tests."""
import socket
from xml.etree import ElementTree

GROUP = "239.255.255.250"
PORT = 13702
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
sock.bind(("", PORT))
sock.setsockopt(
    socket.IPPROTO_IP,
    socket.IP_ADD_MEMBERSHIP,
    socket.inet_aton(GROUP) + socket.inet_aton("127.0.0.1"),
)
while True:
    message, address = sock.recvfrom(65535)
    if address[0] != "127.0.0.1":
        continue
    root = ElementTree.fromstring(message)
    message_id = root.find(".//{*}MessageID").text
    reply = f"""
    <s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope"
      xmlns:a="http://schemas.xmlsoap.org/ws/2004/08/addressing"
      xmlns:dn="http://www.onvif.org/ver10/network/wsdl"
      xmlns:d="http://schemas.xmlsoap.org/ws/2005/04/discovery">
      <s:Header><a:RelatesTo>{message_id}</a:RelatesTo></s:Header>
      <s:Body><d:ProbeMatches><d:ProbeMatch>
        <d:Types>dn:NetworkVideoTransmitter</d:Types>
        <d:Scopes>onvif://www.onvif.org/name/Fixture%20camera</d:Scopes>
        <d:XAddrs>http://127.0.0.1:18080/onvif</d:XAddrs>
      </d:ProbeMatch></d:ProbeMatches></s:Body>
    </s:Envelope>
    """.encode()
    sock.sendto(reply, address)
    sock.sendto(reply, address)
