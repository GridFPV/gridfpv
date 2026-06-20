"""Capture real RotorHazard Socket.IO frames by driving a race on the mock nodes.

Runs inside the RH container against http://localhost:5000. Stages a race, waits
for RACING, simulates laps on two nodes, then dumps every captured frame as JSON.
"""
import json
import time
import socketio

CAPTURE = {}  # event name -> list of payloads (newest few)


def record(event, data):
    CAPTURE.setdefault(event, [])
    if len(CAPTURE[event]) < 6:
        CAPTURE[event].append(data)


sio = socketio.Client(reconnection=False)

for ev in ("race_status", "current_laps", "node_data", "heat_data",
           "frequency_data", "leaderboard", "pass_record"):
    sio.on(ev, handler=(lambda e: (lambda data=None: record(e, data)))(ev))


def main():
    sio.connect("http://localhost:5000", wait_timeout=10)
    time.sleep(2.0)                       # initial bursts on connect

    sio.emit("stage_race")
    time.sleep(6.0)                       # staging -> RACING

    # Generate laps on two nodes with real spacing (ms timestamps differ).
    sio.emit("simulate_lap", {"node": 0}); time.sleep(3.2)   # node0 holeshot
    sio.emit("simulate_lap", {"node": 1}); time.sleep(2.0)   # node1 holeshot
    sio.emit("simulate_lap", {"node": 0}); time.sleep(2.6)   # node0 lap 1
    sio.emit("simulate_lap", {"node": 0}); time.sleep(1.5)   # node0 lap 2

    time.sleep(1.5)
    sio.emit("stop_race")
    time.sleep(1.5)
    sio.disconnect()
    print(json.dumps(CAPTURE, indent=2, default=str))


if __name__ == "__main__":
    main()
