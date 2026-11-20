package dev.nyx.client.screen;

import net.minecraft.client.MinecraftClient;
import net.minecraft.client.gui.DrawContext;
import net.minecraft.client.gui.screen.Screen;
import net.minecraft.client.gui.widget.ButtonWidget;
import net.minecraft.client.gui.widget.SliderWidget;
import net.minecraft.client.option.GraphicsMode;
import net.minecraft.text.Text;

public final class NyxSettingsScreen extends Screen {
    private final Screen parent;

    public NyxSettingsScreen(Screen parent) {
        super(Text.literal("Nyx Client"));
        this.parent = parent;
    }

    @Override
    protected void init() {
        MinecraftClient client = MinecraftClient.getInstance();
        int x = width / 2 - 145;
        int w = 290;

        addDrawableChild(new NyxSlider(x, 86, w, "RENDER DISTANCE", client.options.getViewDistance().getValue(), 2, 32, value -> {
            client.options.getViewDistance().setValue(value);
            client.options.write();
        }, value -> value + " CHUNKS"));

        addDrawableChild(new NyxSlider(x, 137, w, "SIMULATION DISTANCE", client.options.getSimulationDistance().getValue(), 2, 16, value -> {
            client.options.getSimulationDistance().setValue(value);
            client.options.write();
        }, value -> value + " CHUNKS"));

        addDrawableChild(new NyxSlider(x, 188, w, "FRAME RATE LIMIT", client.options.getMaxFps().getValue(), 10, 260, value -> {
            client.options.getMaxFps().setValue(value);
            client.options.write();
        }, value -> value + " FPS"));

        addDrawableChild(ButtonWidget.builder(vsyncText(), button -> {
            client.options.getEnableVsync().setValue(!client.options.getEnableVsync().getValue());
            client.options.write();
            button.setMessage(vsyncText());
        }).dimensions(x, 244, w, 24).build());

        addDrawableChild(ButtonWidget.builder(graphicsText(), button -> {
            GraphicsMode current = client.options.getGraphicsMode().getValue();
            GraphicsMode next = current == GraphicsMode.FAST ? GraphicsMode.FANCY
                : current == GraphicsMode.FANCY ? GraphicsMode.FABULOUS : GraphicsMode.FAST;
            client.options.getGraphicsMode().setValue(next);
            client.options.write();
            button.setMessage(graphicsText());
        }).dimensions(x, 274, w, 24).build());

        addDrawableChild(ButtonWidget.builder(Text.literal("APPLY PERFORMANCE PRESET"), button -> {
            client.options.getViewDistance().setValue(6);
            client.options.getSimulationDistance().setValue(4);
            client.options.getMaxFps().setValue(260);
            client.options.getEnableVsync().setValue(false);
            client.options.getGraphicsMode().setValue(GraphicsMode.FAST);
            client.options.write();
            clearAndInit();
        }).dimensions(x, 310, w, 24).build());

        addDrawableChild(ButtonWidget.builder(Text.literal("BACK"), button -> close())
            .dimensions(x, 342, w, 24).build());
    }

    private Text vsyncText() {
        return Text.literal("VERTICAL SYNC  ·  " + (MinecraftClient.getInstance().options.getEnableVsync().getValue() ? "ON" : "OFF"));
    }

    private Text graphicsText() {
        String name = switch (MinecraftClient.getInstance().options.getGraphicsMode().getValue()) {
            case FAST -> "FAST";
            case FANCY -> "FANCY";
            case FABULOUS -> "FABULOUS";
        };
        return Text.literal("GRAPHICS QUALITY  ·  " + name);
    }

    @Override
    public void render(DrawContext context, int mouseX, int mouseY, float delta) {
        context.fillGradient(0, 0, width, height, 0xF5070B16, 0xF5111728);
        context.fill(width / 2 - 166, 18, width / 2 + 166, 58, 0xAA171F35);
        context.drawCenteredTextWithShadow(textRenderer, Text.literal("◐  NYX CLIENT"), width / 2, 27, 0xFFB9C8FF);
        context.drawCenteredTextWithShadow(textRenderer, Text.literal("VIDEO & PERFORMANCE CONTROL"), width / 2, 44, 0xFF8999C0);
        super.render(context, mouseX, mouseY, delta);
    }

    @Override
    public void close() {
        MinecraftClient.getInstance().setScreen(parent);
    }

    private static final class NyxSlider extends SliderWidget {
        private final String label;
        private final int minimum;
        private final int maximum;
        private final java.util.function.IntConsumer setter;
        private final java.util.function.IntFunction<String> formatter;

        private NyxSlider(int x, int y, int width, String label, int current, int minimum, int maximum,
                          java.util.function.IntConsumer setter, java.util.function.IntFunction<String> formatter) {
            super(x, y, width, 20, Text.empty(), (double) (current - minimum) / (maximum - minimum));
            this.label = label;
            this.minimum = minimum;
            this.maximum = maximum;
            this.setter = setter;
            this.formatter = formatter;
            updateMessage();
        }

        @Override
        protected void updateMessage() {
            int value = minimum + (int) Math.round(this.value * (maximum - minimum));
            setMessage(Text.literal(label + "  ·  " + formatter.apply(value)));
        }

        @Override
        protected void applyValue() {
            int value = minimum + (int) Math.round(this.value * (maximum - minimum));
            setter.accept(value);
        }
    }
}
