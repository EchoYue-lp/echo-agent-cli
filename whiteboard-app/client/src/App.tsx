import React, { useState, useEffect, useRef } from 'react';

function App() {
  const [roomID, setRoomID] = useState<string>('');
  const [isConnected, setIsConnected] = useState<boolean>(false);
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    // Initialize canvas
    const canvas = canvasRef.current;
    if (!canvas) return;

    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    // Set canvas size
    const resizeCanvas = () => {
      canvas.width = window.innerWidth * 0.8;
      canvas.height = window.innerHeight * 0.7;
      ctx.clearRect(0, 0, canvas.width, canvas.height);
    };

    resizeCanvas();
    window.addEventListener('resize', resizeCanvas);

    return () => window.removeEventListener('resize', resizeCanvas);
  }, []);

  const handleConnect = () => {
    if (!roomID.trim()) return;
    // In a real app, this would connect to Socket.IO
    setIsConnected(true);
  };

  return (
    <div className="min-h-screen bg-gray-50 p-4">
      <div className="max-w-6xl mx-auto">
        <header className="text-center mb-8">
          <h1 className="text-4xl font-bold text-gray-800 mb-2">Collaborative Whiteboard</h1>
          <p className="text-gray-600">Real-time drawing with memory storage</p>
        </header>

        {!isConnected ? (
          <div className="bg-white rounded-xl shadow-lg p-6 max-w-md mx-auto">
            <h2 className="text-xl font-semibold text-gray-700 mb-4">Join a Room</h2>
            <div className="flex gap-2 mb-4">
              <input
                type="text"
                value={roomID}
                onChange={(e) => setRoomID(e.target.value)}
                placeholder="Enter room ID"
                className="flex-1 px-4 py-2 border border-gray-300 rounded-lg focus:ring-2 focus:ring-blue-500 focus:border-transparent"
              />
              <button
                onClick={handleConnect}
                className="px-6 py-2 bg-blue-600 text-white rounded-lg hover:bg-blue-700 transition-colors"
              >
                Connect
              </button>
            </div>
            <p className="text-sm text-gray-500">
              Create a new room by entering any unique ID, or join an existing one.
            </p>
          </div>
        ) : (
          <div className="bg-white rounded-xl shadow-lg p-6">
            <div className="flex justify-between items-center mb-4">
              <h2 className="text-xl font-semibold text-gray-700">Room: {roomID}</h2>
              <button 
                className="px-4 py-2 bg-gray-200 text-gray-700 rounded-lg hover:bg-gray-300 transition-colors"
                onClick={() => setIsConnected(false)}
              >
                Disconnect
              </button>
            </div>
            <div className="border-2 border-gray-300 rounded-lg overflow-hidden bg-white">
              <canvas 
                ref={canvasRef} 
                className="w-full h-96 bg-white cursor-crosshair"
                style={{ touchAction: 'none' }}
              />
            </div>
            <div className="mt-4 flex gap-2">
              <button className="px-4 py-2 bg-gray-200 text-gray-700 rounded-lg hover:bg-gray-300 transition-colors">
                Clear Canvas
              </button>
              <button className="px-4 py-2 bg-blue-600 text-white rounded-lg hover:bg-blue-700 transition-colors">
                Save Drawing
              </button>
            </div>
          </div>
        )}

        <footer className="mt-12 text-center text-gray-500 text-sm">
          <p>Whiteboard App • Memory-only storage • No database required</p>
        </footer>
      </div>
    </div>
  );
}

export default App;