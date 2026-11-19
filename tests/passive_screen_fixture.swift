// Render a synthetic app screen for the native Vision/recorder/controls integration test.
import AppKit

let width = 1280, height = 800
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: width, pixelsHigh: height,
    bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
    bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)!
func color(_ r: Double, _ g: Double, _ b: Double) -> NSColor {
    NSColor(srgbRed: r/255, green: g/255, blue: b/255, alpha: 1)
}
func fill(_ x: Int, _ y: Int, _ w: Int, _ h: Int, _ ink: NSColor) {
    ink.setFill()
    NSBezierPath(rect: NSRect(x: x, y: y, width: w, height: h)).fill()
}
func label(_ text: String, _ x: Int, _ y: Int, _ font: NSFont, _ ink: NSColor) {
    (text as NSString).draw(at: NSPoint(x: x, y: y), withAttributes: [.font: font, .foregroundColor: ink])
}
let paper = color(251,248,238), ink = color(37,54,43), green = color(52,87,71)
fill(0,0,width,height,paper)
fill(0,704,width,96,green)
label("Print studio",70,731,.systemFont(ofSize: 33,weight: .medium),.white)
label("Autumn edition",922,737,.systemFont(ofSize: 27),.white)
label("Field Notes",68,535,NSFont(name:"Georgia",size:105)!,ink)
label("Cover proof",75,477,.systemFont(ofSize: 37),green)
for (i, pair) in [("Paper","Warm white"),("Binding","Forest green thread"),("Press check","Friday at 10")].enumerated() {
    let y=338-i*100
    fill(72,y+64,1128,2,color(205,210,194))
    label(pair.0,75,y,.systemFont(ofSize: 35),ink)
    label(pair.1,667,y,.systemFont(ofSize: 35),ink)
}
NSGraphicsContext.restoreGraphicsState()
try rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
