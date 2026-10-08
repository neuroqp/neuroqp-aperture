import ij.ImagePlus;
import ij.ImageStack;
import ij.gui.Overlay;
import ij.gui.PointRoi;
import ij.gui.Roi;
import ij.io.FileSaver;
import ij.io.FileInfo;
import ij.io.FileOpener;
import ij.io.RoiDecoder;
import ij.io.TiffDecoder;
import ij.process.ByteProcessor;
import ij.process.ColorProcessor;
import ij.process.FloatPolygon;
import ij.process.FloatProcessor;
import ij.process.ShortProcessor;
import java.awt.Color;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.Arrays;

class ImageJInterop {
    private static void check(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }

    private static PointRoi points() {
        PointRoi roi = new PointRoi(new float[]{12.25f, 3.5f}, new float[]{16.75f, 9.25f}, 2);
        roi.setPointType(2);
        roi.setSize(3);
        roi.setStrokeColor(new Color(0xffff8000, true));
        roi.setShowLabels(true);
        roi.setCounters(new int[]{2, 4});
        roi.setName("synthetic-active");
        roi.setProperty("source", "synthetic");
        return roi;
    }

    private static void save(ImagePlus image, Path path) {
        FileSaver saver = new FileSaver(image);
        check(image.getStackSize() > 1 ? saver.saveAsTiffStack(path.toString()) : saver.saveAsTiff(path.toString()), "save failed");
    }

    private static void generate(Path directory) {
        ImageStack stack = new ImageStack(32, 24);
        for (int slice = 0; slice < 3; slice++) {
            short[] pixels = new short[32 * 24];
            for (int i = 0; i < pixels.length; i++) pixels[i] = (short)(i * 67 + slice * 1234);
            stack.addSlice("slice-" + slice, new ShortProcessor(32, 24, pixels, null));
        }
        float[] floats = new float[32 * 24];
        int[] rgb = new int[floats.length];
        for (int i = 0; i < floats.length; i++) {
            floats[i] = (i - 300) * 0.125f;
            rgb[i] = (i * 7919) & 0xffffff;
        }
        ImagePlus[] images = {new ImagePlus("stack", stack), new ImagePlus("float", new FloatProcessor(32, 24, floats)), new ImagePlus("rgb", new ColorProcessor(32, 24, rgb))};
        String[] names = {"gray16-stack.tif", "float32.tif", "rgb.tif"};
        for (int i = 0; i < images.length; i++) {
            ImagePlus image = images[i];
            image.getCalibration().pixelWidth = 0.325;
            image.getCalibration().pixelHeight = 0.425;
            image.getCalibration().setUnit("um");
            image.setProperty("Info", "synthetic ImageJ metadata");
            image.setRoi(points());
            Roi rectangle = new Roi(1, 2, 8, 9);
            rectangle.setName("untouched-rectangle");
            rectangle.setStrokeColor(Color.magenta);
            rectangle.setStrokeWidth(2.5);
            Overlay overlay = new Overlay(rectangle);
            PointRoi extra = points();
            extra.setName("editable-overlay");
            extra.setCounters(new int[]{(2 << 8) | 2, (1 << 8) | 4});
            extra.setPointType(3);
            extra.setSize(5);
            extra.setStrokeColor(Color.cyan);
            overlay.add(extra);
            image.setOverlay(overlay);
            save(image, directory.resolve(names[i]));
        }
        save(new ImagePlus("plain", new ByteProcessor(32, 24)), directory.resolve("plain.tif"));
    }

    private static ImagePlus open(Path path) throws Exception {
        FileInfo info = new TiffDecoder(path.getParent().toString() + java.io.File.separator, path.getFileName().toString()).getTiffInfo()[0];
        byte[] roi = info.roi;
        // FileOpener tries to select the toolbar for multicounter points, which
        // requires a desktop. Use its own RoiDecoder path without that UI action.
        info.roi = null;
        ImagePlus image = new FileOpener(info).openImage();
        if (roi != null) image.setRoi(RoiDecoder.openFromByteArray(roi));
        info.roi = roi;
        return image;
    }

    private static void verify(Path directory) throws Exception {
        for (String name : new String[]{"gray16-stack.tif", "float32.tif", "rgb.tif", "plain.tif"}) {
            ImagePlus original = open(directory.resolve(name));
            ImagePlus saved = open(directory.resolve("saved-" + name));
            check(original != null && saved != null, "ImageJ open failed");
            check(original.getWidth() == 32 && original.getHeight() == 24 && original.getStackSize() == (name.equals("gray16-stack.tif") ? 3 : 1), "synthetic image dimensions wrong");
            check(original.getWidth() == saved.getWidth() && original.getHeight() == saved.getHeight() && original.getBitDepth() == saved.getBitDepth() && original.getStackSize() == saved.getStackSize(), "image dimensions/type changed");
            for (int s = 1; s <= original.getStackSize(); s++) {
                Object a = original.getStack().getPixels(s), b = saved.getStack().getPixels(s);
                boolean equal = a instanceof short[] ? Arrays.equals((short[])a, (short[])b) : a instanceof float[] ? Arrays.equals((float[])a, (float[])b) : a instanceof int[] ? Arrays.equals((int[])a, (int[])b) : Arrays.equals((byte[])a, (byte[])b);
                check(equal, "pixels changed: " + name + " slice " + s);
            }
            check(saved.getRoi() instanceof PointRoi, "active ROI is not editable PointRoi");
            PointRoi active = (PointRoi)saved.getRoi();
            FloatPolygon xy = active.getFloatPolygon();
            check(xy.npoints == 2 && xy.xpoints[0] == 12.25f && xy.ypoints[0] == 16.75f && xy.xpoints[1] == 7.125f && xy.ypoints[1] == 8.875f, "coordinates changed");
            check(active.getPointType() == 2 && active.getSize() == 3 && active.getStrokeColor().getRGB() == 0xffff8000 && active.getShowLabels(), "point style changed");
            check(Arrays.equals(active.getCounters(), new int[]{2, 4}), "counters changed");
            if (!name.equals("plain.tif")) {
                check(active.getName().equals("synthetic-active") && active.getProperty("source").equals("synthetic"), "ROI properties lost");
                check(saved.getCalibration().pixelWidth == original.getCalibration().pixelWidth && saved.getCalibration().pixelHeight == original.getCalibration().pixelHeight && saved.getCalibration().getUnit().equals(original.getCalibration().getUnit()), "calibration changed");
                check(saved.getProperty("Info").equals(original.getProperty("Info")), "info changed");
                Overlay overlay = saved.getOverlay();
                check(overlay != null && overlay.size() == 2, "overlay missing");
                Roi rectangle = overlay.get(0);
                check(rectangle.getType() == Roi.RECTANGLE && rectangle.getName().equals("untouched-rectangle") && rectangle.getStrokeColor().equals(Color.magenta) && rectangle.getStrokeWidth() == 2.5 && rectangle.getBounds().equals(original.getOverlay().get(0).getBounds()), "unsupported overlay changed");
                PointRoi extra = (PointRoi)overlay.get(1);
                check(extra.getFloatPolygon().xpoints[0] == 5.25f && extra.getPointType() == 3 && extra.getSize() == 5 && extra.getStrokeColor().equals(Color.cyan), "point overlay changed");
                check(Arrays.equals(extra.getCounters(), new int[]{(2 << 8) | 2, (1 << 8) | 4}), "point overlay counters/positions changed");
            }
            if (name.equals("float32.tif")) {
                active.setCounter(4);
                active.addPoint(19.25, 20.75);
                check(active.getFloatPolygon().npoints == 3, "ROI could not be edited");
                save(saved, directory.resolve("imagej-edited.tif"));
            }
        }
    }

    public static void main(String[] args) throws Exception {
        Path directory = Paths.get(args[1]);
        if (args[0].equals("generate")) generate(directory);
        else verify(directory);
    }
}
