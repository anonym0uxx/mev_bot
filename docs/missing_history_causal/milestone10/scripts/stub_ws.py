import asyncio,sys,websockets
async def h(ws):
    try:
        async for _ in ws: pass
    except Exception: pass
async def main():
    async with websockets.serve(h,'127.0.0.1',int(sys.argv[1])): await asyncio.Future()
asyncio.run(main())
