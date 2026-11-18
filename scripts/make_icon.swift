// Developer-only vector asset generator. Not compiled into or run by Daybook.
import AppKit
let out = CommandLine.arguments[1]
let size = 1024
let rep = NSBitmapImageRep(bitmapDataPlanes:nil,pixelsWide:size,pixelsHigh:size,bitsPerSample:8,samplesPerPixel:4,hasAlpha:true,isPlanar:false,colorSpaceName:.deviceRGB,bytesPerRow:0,bitsPerPixel:0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep:rep)
func color(_ r:CGFloat,_ g:CGFloat,_ b:CGFloat)->NSColor {NSColor(calibratedRed:r/255,green:g/255,blue:b/255,alpha:1)}
let tile=NSBezierPath(roundedRect:NSRect(x:64,y:64,width:896,height:896),xRadius:205,yRadius:205)
NSGradient(starting:color(40,65,65),ending:color(15,28,33))!.draw(in:tile,angle:270)
let rim=NSBezierPath(roundedRect:NSRect(x:66,y:66,width:892,height:892),xRadius:203,yRadius:203)
color(67,91,88).withAlphaComponent(0.6).setStroke();rim.lineWidth=3;rim.stroke()
NSGraphicsContext.saveGraphicsState()
let shadow=NSShadow();shadow.shadowColor=NSColor.black.withAlphaComponent(0.35);shadow.shadowBlurRadius=30;shadow.shadowOffset=NSSize(width:0,height:-18);shadow.set()
let book=NSBezierPath(roundedRect:NSRect(x:253,y:221,width:518,height:596),xRadius:56,yRadius:56)
NSGradient(starting:color(240,247,229),ending:color(207,226,211))!.draw(in:book,angle:270)
NSGraphicsContext.restoreGraphicsState()
let spine=NSBezierPath();spine.move(to:NSPoint(x:321,y:244));spine.line(to:NSPoint(x:321,y:793));spine.lineWidth=4;color(158,184,166).setStroke();spine.stroke()
// Bookmark: a quiet coral accent, distinct from a recording status indicator.
let ribbon=NSBezierPath();ribbon.move(to:NSPoint(x:640,y:817));ribbon.line(to:NSPoint(x:699,y:817));ribbon.line(to:NSPoint(x:699,y:704));ribbon.line(to:NSPoint(x:670,y:726));ribbon.line(to:NSPoint(x:640,y:704));ribbon.close();color(228,132,112).setFill();ribbon.fill()
for (y,w) in [(681.0,175.0),(636.0,240.0),(338.0,230.0)] {let line=NSBezierPath();line.move(to:NSPoint(x:374,y:y));line.line(to:NSPoint(x:374+w,y:y));line.lineWidth=13;line.lineCapStyle = .round;color(137,165,149).withAlphaComponent(0.65).setStroke();line.stroke()}
let capsule=NSBezierPath(roundedRect:NSRect(x:195,y:405,width:634,height:166),xRadius:57,yRadius:57)
NSGradient(starting:color(140,230,190),ending:color(91,196,159))!.draw(in:capsule,angle:270)
let wave=NSBezierPath();wave.move(to:NSPoint(x:260,y:487))
for p in [(326,487),(355,515),(390,459),(435,533),(480,438),(523,533),(563,460),(598,513),(628,487),(763,487)] {wave.line(to:NSPoint(x:p.0,y:p.1))}
wave.lineWidth=18;wave.lineCapStyle = .round;wave.lineJoinStyle = .round;color(23,64,56).setStroke();wave.stroke()
NSGraphicsContext.restoreGraphicsState()
try rep.representation(using:.png,properties:[:])!.write(to:URL(fileURLWithPath:out))
