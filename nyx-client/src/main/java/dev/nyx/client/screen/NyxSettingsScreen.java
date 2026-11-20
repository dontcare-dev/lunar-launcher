package dev.nyx.client.screen;

import net.minecraft.client.MinecraftClient;
import net.minecraft.client.gui.DrawContext;
import net.minecraft.client.gui.screen.GameMenuScreen;
import net.minecraft.client.gui.screen.Screen;
import net.minecraft.client.gui.widget.ButtonWidget;
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
        int x = width / 2 - 116;
        int y = 62;
        int w = 232;

        addDrawableChild(ButtonWidget.builder(renderDistanceText(), button -> {
            int current = client.options.getViewDistance().getValue();
            int next = current >= 32 ? 2 : Math.min(32, current + 2);
            client.options.getViewDistance().setValue(next);
            button.setMessage(renderDistanceText());
            client.options.write();
        }).dimensions(x, y, w, 22).build());

        addDrawableChild(ButtonWidget.builder(simulationDistanceText(), button -> {
            int current = client.options.getSimulationDistance().getValue();
            int next = current >= 16 ? 2 : Math.min(16, current + 2);
            client.options.getSimulationDistance().setValue(next);
            button.setMessage(simulationDistanceText());
            client.options.write();
        }).dimensions(x, y + 29, w, 22).build());

        addDrawableChild(ButtonWidget.builder(frameRateText(), button -> {
            int current = client.options.getMaxFps().getValue();
            int next = current < 120 ? 120 : current < 165 ? 165 : current < 240 ? 240 : 260;
            if (current >= 260) next = 60;
            client.options.getMaxFps().setValue(next);
            button.setMessage(frameRateText());
            client.options.write();
        }).dimensions(x, y + 58, w, 22).build());

        addDrawableChild(ButtonWidget.builder(vsyncText(), button -> {
            boolean next = !client.options.getEnableVsync().getValue();
            client.options.getEnableVsync().setValue(next);
            button.setMessage(vsyncText());
            client.options.write();
        }).dimensions(x, y + 87, w, 22).build());

        addDrawableChild(ButtonWidget.builder(graphicsText(), button -> {
            GraphicsMode current = client.options.getGraphicsMode().getValue();
            GraphicsMode next = current == GraphicsMode.FAST ? GraphicsMode.FANCY
                : current == GraphicsMode.FANCY ? GraphicsMode.FABULOUS : GraphicsMode.FAST;
            client.options.getGraphicsMode().setValue(next);
            button.setMessage(graphicsText());
            client.options.write();
        }).dimensions(x, y + 116, w, 22).build());

        addDrawableChild(ButtonWidget.builder(Text.literal("APPLY NYX PERFORMANCE PRESET"), button -> {
            client.options.getViewDistance().setValue(6);
            client.options.getSimulationDistance().setValue(4);
            client.options.getMaxFps().setValue(260);
            client.options.getEnableVsync().setValue(false);
            client.options.getGraphicsMode().setValue(GraphicsMode.FAST);
            client.options.write();
            clearAndInit();
        }).dimensions(x, y + 151, w, 22).build());

        addDrawableChild(ButtonWidget.builder(Text.literal("BACK"), button -> close())
            .dimensions(x, y + 184, w, 22).build());
    }

    private Text renderDistanceText() {
        return Text.literal("RENDER DISTANCE  ·  " + MinecraftClient.getInstance().options.getViewDistance().getValue() + " CHUNKS");
    }

    private Text simulationDistanceText() {
        return Text.literal("SIMULATION DISTANCE  ·  " + MinecraftClient.getInstance().options.getSimulationDistance().getValue() + " CHUNKS");
    }

    private Text frameRateText() {
        int fps = MinecraftClient.getInstance().options.getMaxFps().getValue();
        return Text.literal("FPS LIMIT  ·  " + (fps >= 260 ? "UNLIMITED" : fps));
    }

    private Text vsyncText() {
        return Text.literal("VERTICAL SYNC  ·  " + (MinecraftClient.getInstance().options.getEnableVsync().getValue() ? "ON" : "OFF"));
    }

    private Text graphicsText() {
        return Text.literal("GRAPHICS  ·  " + MinecraftClient.getInstance().options.getGraphicsMode().getValue().getName().getString().toUpperCase());
    }

    @Override
    public void render(DrawContext context, int mouseX, int mouseY, float delta) {
        context.fillGradient(0, 0, width, height, 0xF5070B16, 0xF5111728);
        context.fill(width / 2 - 150, 18, width / 2 + 150, 48, 0xAA171F35);
        context.drawCenteredTextWithShadow(textRenderer, Text.literal("◐  NYX CLIENT"), width / 2, 28, 0xFFB9C8FF);
        context.drawCenteredTextWithShadow(textRenderer, Text.literal("CUSTOM VIDEO CONTROL"), width / 2, 52, 0xFF8999C0);
        super.render(context, mouseX, mouseY, delta);
    }

    @Override
    public void close() {
        MinecraftClient.getInstance().setScreen(parent);
    }
}
