package dev.nyx.client.mixin;

import dev.nyx.client.screen.NyxSettingsScreen;
import net.minecraft.client.MinecraftClient;
import net.minecraft.client.gui.screen.GameMenuScreen;
import net.minecraft.client.gui.DrawContext;
import net.minecraft.client.gui.widget.ButtonWidget;
import net.minecraft.text.Text;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(GameMenuScreen.class)
public abstract class GameMenuScreenMixin {
    @Inject(method = "render", at = @At("HEAD"))
    private void nyx$drawPauseBranding(DrawContext context, int mouseX, int mouseY, float delta, CallbackInfo ci) {
        context.fill(8, 8, 142, 38, 0xC9080D19);
        context.drawText(MinecraftClient.getInstance().textRenderer, "NYX CLIENT", 20, 19, 0xFFB9C8FF, true);
        context.fill(20, 30, 130, 31, 0xFF829CFF);
    }

    @Inject(method = "init", at = @At("TAIL"))
    private void nyx$addClientButton(CallbackInfo ci) {
        int width = MinecraftClient.getInstance().getWindow().getScaledWidth();
        ((ScreenAccessor) (Object) this).nyx$addDrawableChild(ButtonWidget.builder(Text.literal("NYX CLIENT"), button ->
            MinecraftClient.getInstance().setScreen(new NyxSettingsScreen((GameMenuScreen) (Object) this))
        ).dimensions(width - 118, 10, 108, 22).build());
    }
}
