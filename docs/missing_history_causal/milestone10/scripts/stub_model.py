BUY="DECISION: BUY\nSIZE: SMALL\nPRICE LIMIT: 0.5\nINVALIDATION: none\nEVIDENCE: flow sustained"
import http.server,json,sys,threading
LOG=sys.argv[2]
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(s):
        n=int(s.headers.get('content-length',0)); b=json.loads(s.rfile.read(n))
        user=b['messages'][1]['content']
        with open(LOG,'a') as f: f.write(json.dumps({'user':user})+'\n')
        mgmt=user.startswith('Decide the next action for a position you already hold')
        text="DECISION: HOLD\nINVALIDATION: none\nEVIDENCE: x" if mgmt else BUY
        out=json.dumps({"choices":[{"message":{"content":text},"finish_reason":"stop"}]}).encode()
        s.send_response(200); s.send_header('content-type','application/json'); s.send_header('content-length',str(len(out))); s.end_headers(); s.wfile.write(out)
    def log_message(*a): pass
http.server.ThreadingHTTPServer(('127.0.0.1',int(sys.argv[1])),H).serve_forever()
